//! CPU evaluator of naga IR for fragment shaders.
//!
//! A tree-walking interpreter over naga's expression/statement arenas: one [`Exec`] per
//! thread evaluates `main` once per pixel. Values are small enums (scalars, vectors of up
//! to 4 lanes, matrices of up to 4x4 inline; arrays/structs boxed); pointers are
//! `(root, access path)` pairs into local or global storage. Texture calls of the
//! rewritten dialect (`_pen_tex*`) are answered by a [`Textures`] host.
//!
//! Emitted expressions are re-evaluated each time their `Emit` statement runs (loops);
//! other expressions (literals, constants, pointers, constant-folded composites) are
//! evaluated on first use and cached per call frame.

use naga::{
    BinaryOperator as Bo, Expression as E, Function, Handle, Literal, MathFunction as Mf, Module,
    RelationalFunction, ScalarKind, Statement as S, TypeInner, UnaryOperator,
};
use smallvec::SmallVec;

/// A runtime value.
#[derive(Clone, Debug, PartialEq)]
pub enum V {
    Void,
    F(f32),
    I(i32),
    U(u32),
    B(bool),
    Fv([f32; 4], u8),
    Iv([i32; 4], u8),
    Uv([u32; 4], u8),
    Bv([bool; 4], u8),
    /// Column-major, `cols` x `rows`.
    M([f32; 16], u8, u8),
    /// Arrays and structs.
    A(Box<[V]>),
    P(Ptr),
}

/// Pointer: storage root plus access path.
#[derive(Clone, Debug, PartialEq)]
pub struct Ptr {
    pub root: Root,
    pub path: SmallVec<[u16; 4]>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Root {
    Global(u32),
    /// (frame depth, local index)
    Local(u32, u32),
}

impl V {
    pub fn as_f32(&self) -> f32 {
        match self {
            V::F(v) => *v,
            V::I(v) => *v as f32,
            V::U(v) => *v as f32,
            V::B(b) => *b as u8 as f32,
            V::Fv(v, _) => v[0],
            _ => 0.0,
        }
    }
    pub fn as_i32(&self) -> i32 {
        match self {
            V::I(v) => *v,
            V::U(v) => *v as i32,
            V::F(v) => *v as i32,
            V::B(b) => *b as i32,
            _ => 0,
        }
    }
    fn as_bool(&self) -> bool {
        match self {
            V::B(b) => *b,
            V::I(v) => *v != 0,
            V::U(v) => *v != 0,
            V::F(v) => *v != 0.0,
            _ => false,
        }
    }
    /// Float lanes (scalar = 1 lane, `n` = 0 marks a scalar).
    fn fl(&self) -> Option<([f32; 4], u8)> {
        Some(match self {
            V::F(v) => ([*v; 4], 0),
            V::Fv(v, n) => (*v, *n),
            _ => return None,
        })
    }
    pub fn vec4(&self) -> [f32; 4] {
        match self {
            V::Fv(v, _) => *v,
            V::F(v) => [*v, 0.0, 0.0, 0.0],
            _ => [0.0; 4],
        }
    }
}

fn fv(v: [f32; 4], n: u8) -> V {
    if n == 0 { V::F(v[0]) } else { V::Fv(v, n) }
}

/// Texture host for `texture2D` / `texture2DLod` / `textureSize`.
pub trait Textures: Sync {
    /// Samples slot `slot` at `uv` (GL convention: v up) at mip level `lod`.
    fn sample(&self, slot: i32, uv: [f32; 2], lod: f32) -> [f32; 4];
    fn size(&self, slot: i32) -> [i32; 2];
}

/// No textures (every sample is transparent black).
pub struct NoTextures;

impl Textures for NoTextures {
    fn sample(&self, _: i32, _: [f32; 2], _: f32) -> [f32; 4] {
        [0.0; 4]
    }
    fn size(&self, _: i32) -> [i32; 2] {
        [1, 1]
    }
}

/// Why a pixel could not be evaluated.
#[derive(Debug, Clone, PartialEq)]
pub enum EvalError {
    /// Loop iteration budget exhausted.
    TooLong,
    /// Unsupported IR (message).
    Unsupported(String),
}

enum Flow {
    Next,
    Break,
    Continue,
    Return(V),
    Kill,
}

/// Host functions of the rewritten dialect.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Host {
    None,
    Tex,
    TexLod,
    TexSize,
}

struct Frame {
    vals: Vec<Option<V>>,
    locals: Vec<V>,
    args: Vec<V>,
}

/// Compiled program: the module plus per-function host tags and constant values.
pub struct Program {
    pub module: Module,
    host: Vec<Host>,
    consts: Vec<V>,
    globals_init: Vec<V>,
    /// Index of the `_pen_frag_coord` / `_pen_frag_color` globals.
    frag_coord: Option<u32>,
    frag_color: Option<u32>,
}

/// Loop iterations allowed per pixel.
pub const MAX_ITERATIONS: u64 = 1 << 20;

impl Program {
    pub fn new(module: Module) -> Result<Program, EvalError> {
        let host = module
            .functions
            .iter()
            .map(|(_, f)| match f.name.as_deref() {
                Some("_pen_tex") => Host::Tex,
                Some("_pen_tex_lod") => Host::TexLod,
                Some("_pen_tex_size") => Host::TexSize,
                _ => Host::None,
            })
            .collect();
        if module.entry_points.is_empty() {
            return Err(EvalError::Unsupported("the shader has no main()".into()));
        }
        // Global expressions (constants, global initializers).
        let mut gvals: Vec<Option<V>> = vec![None; module.global_expressions.len()];
        for (h, _) in module.global_expressions.iter() {
            let v = const_eval(&module, h, &mut gvals)?;
            gvals[h.index()] = Some(v);
        }
        let consts = module
            .constants
            .iter()
            .map(|(_, c)| gvals[c.init.index()].clone().unwrap_or(V::Void))
            .collect();
        let mut frag_coord = None;
        let mut frag_color = None;
        let globals_init = module
            .global_variables
            .iter()
            .map(|(h, g)| {
                match g.name.as_deref() {
                    Some("_pen_frag_coord") => frag_coord = Some(h.index() as u32),
                    Some("_pen_frag_color") => frag_color = Some(h.index() as u32),
                    _ => {}
                }
                match g.init {
                    Some(i) => gvals[i.index()].clone().unwrap_or(V::Void),
                    None => zero(&module, g.ty),
                }
            })
            .collect();
        Ok(Program {
            module,
            host,
            consts,
            globals_init,
            frag_coord,
            frag_color,
        })
    }

    /// Index of a global variable by name.
    pub fn global(&self, name: &str) -> Option<(u32, Handle<naga::Type>)> {
        self.module
            .global_variables
            .iter()
            .find(|(_, g)| g.name.as_deref() == Some(name))
            .map(|(h, g)| (h.index() as u32, g.ty))
    }

    /// Overrides the initial value of a global (uniform binding).
    pub fn set_global(&mut self, index: u32, v: V) {
        if let Some(g) = self.globals_init.get_mut(index as usize) {
            *g = v;
        }
    }

    pub fn globals_init(&self) -> &[V] {
        &self.globals_init
    }
}

/// Zero value of a type.
pub fn zero(m: &Module, ty: Handle<naga::Type>) -> V {
    match &m.types[ty].inner {
        TypeInner::Scalar(s) => match s.kind {
            ScalarKind::Float | ScalarKind::AbstractFloat => V::F(0.0),
            ScalarKind::Sint | ScalarKind::AbstractInt => V::I(0),
            ScalarKind::Uint => V::U(0),
            ScalarKind::Bool => V::B(false),
        },
        TypeInner::Vector { size, scalar } => {
            let n = *size as u8;
            match scalar.kind {
                ScalarKind::Float | ScalarKind::AbstractFloat => V::Fv([0.0; 4], n),
                ScalarKind::Sint | ScalarKind::AbstractInt => V::Iv([0; 4], n),
                ScalarKind::Uint => V::Uv([0; 4], n),
                ScalarKind::Bool => V::Bv([false; 4], n),
            }
        }
        TypeInner::Matrix { columns, rows, .. } => V::M([0.0; 16], *columns as u8, *rows as u8),
        TypeInner::Array { base, size, .. } => {
            let n = match size {
                naga::ArraySize::Constant(n) => n.get() as usize,
                _ => 0,
            };
            V::A((0..n).map(|_| zero(m, *base)).collect())
        }
        TypeInner::Struct { members, .. } => {
            V::A(members.iter().map(|mb| zero(m, mb.ty)).collect())
        }
        _ => V::Void,
    }
}

fn unsupported<T>(what: &str) -> Result<T, EvalError> {
    Err(EvalError::Unsupported(what.to_string()))
}

/// Evaluates a global (constant) expression.
fn const_eval(m: &Module, h: Handle<E>, cache: &mut Vec<Option<V>>) -> Result<V, EvalError> {
    if let Some(v) = &cache[h.index()] {
        return Ok(v.clone());
    }
    let get = |x: Handle<E>, cache: &mut Vec<Option<V>>| -> Result<V, EvalError> {
        let v = const_eval(m, x, cache)?;
        cache[x.index()] = Some(v.clone());
        Ok(v)
    };
    let v = match &m.global_expressions[h] {
        E::Literal(l) => literal(l),
        E::Constant(c) => get(m.constants[*c].init, cache)?,
        E::ZeroValue(t) => zero(m, *t),
        E::Compose { ty, components } => {
            let mut vals = Vec::with_capacity(components.len());
            for c in components {
                vals.push(get(*c, cache)?);
            }
            compose(m, *ty, vals)
        }
        E::Splat { size, value } => splat(get(*value, cache)?, *size as u8),
        E::Swizzle {
            size,
            vector,
            pattern,
        } => swizzle(&get(*vector, cache)?, *size as u8, pattern),
        E::AccessIndex { base, index } => index_value(&get(*base, cache)?, *index as usize),
        E::Access { base, index } => {
            let i = get(*index, cache)?.as_i32().max(0) as usize;
            index_value(&get(*base, cache)?, i)
        }
        E::Unary { op, expr } => unary(*op, get(*expr, cache)?),
        E::Binary { op, left, right } => binary(*op, get(*left, cache)?, get(*right, cache)?),
        E::Select {
            condition,
            accept,
            reject,
        } => select(
            get(*condition, cache)?,
            get(*accept, cache)?,
            get(*reject, cache)?,
        ),
        E::Relational { fun, argument } => relational(*fun, get(*argument, cache)?),
        E::Math {
            fun,
            arg,
            arg1,
            arg2,
            arg3: _,
        } => {
            let a = get(*arg, cache)?;
            let b = match arg1 {
                Some(x) => Some(get(*x, cache)?),
                None => None,
            };
            let c = match arg2 {
                Some(x) => Some(get(*x, cache)?),
                None => None,
            };
            math(*fun, a, b, c)?
        }
        E::As {
            expr,
            kind,
            convert,
        } => cast(get(*expr, cache)?, *kind, convert.is_some()),
        E::Override(_) => V::F(0.0),
        other => return unsupported(&format!("constant expression {other:?}")),
    };
    Ok(v)
}

fn literal(l: &Literal) -> V {
    match *l {
        Literal::F64(v) => V::F(v as f32),
        Literal::F32(v) => V::F(v),
        Literal::F16(v) => V::F(f32::from(v)),
        Literal::U32(v) => V::U(v),
        Literal::I32(v) => V::I(v),
        Literal::U16(v) => V::U(v as u32),
        Literal::I16(v) => V::I(v as i32),
        Literal::U64(v) => V::U(v as u32),
        Literal::I64(v) => V::I(v as i32),
        Literal::Bool(v) => V::B(v),
        Literal::AbstractInt(v) => V::I(v as i32),
        Literal::AbstractFloat(v) => V::F(v as f32),
    }
}

/// Builds a composite of type `ty` from its components.
pub fn compose(m: &Module, ty: Handle<naga::Type>, comps: Vec<V>) -> V {
    match &m.types[ty].inner {
        TypeInner::Vector { size, scalar } => {
            let n = *size as usize;
            match scalar.kind {
                ScalarKind::Float | ScalarKind::AbstractFloat => {
                    let mut out = [0.0f32; 4];
                    let mut i = 0;
                    for c in &comps {
                        for x in lanes_f32(c) {
                            if i < n {
                                out[i] = x;
                                i += 1;
                            }
                        }
                    }
                    V::Fv(out, n as u8)
                }
                ScalarKind::Sint | ScalarKind::AbstractInt => {
                    let mut out = [0i32; 4];
                    let mut i = 0;
                    for c in &comps {
                        for x in lanes_f32(c) {
                            if i < n {
                                out[i] = x as i32;
                                i += 1;
                            }
                        }
                    }
                    V::Iv(out, n as u8)
                }
                ScalarKind::Uint => {
                    let mut out = [0u32; 4];
                    let mut i = 0;
                    for c in &comps {
                        for x in lanes_f32(c) {
                            if i < n {
                                out[i] = x as u32;
                                i += 1;
                            }
                        }
                    }
                    V::Uv(out, n as u8)
                }
                ScalarKind::Bool => {
                    let mut out = [false; 4];
                    let mut i = 0;
                    for c in &comps {
                        for x in lanes_f32(c) {
                            if i < n {
                                out[i] = x != 0.0;
                                i += 1;
                            }
                        }
                    }
                    V::Bv(out, n as u8)
                }
            }
        }
        TypeInner::Matrix { columns, rows, .. } => {
            let (c, r) = (*columns as usize, *rows as usize);
            let mut out = [0.0f32; 16];
            let flat: Vec<f32> = comps.iter().flat_map(lanes_f32).collect();
            for col in 0..c {
                for row in 0..r {
                    out[col * 4 + row] = flat.get(col * r + row).copied().unwrap_or(0.0);
                }
            }
            V::M(out, c as u8, r as u8)
        }
        TypeInner::Scalar(_) => comps.into_iter().next().unwrap_or(V::Void),
        _ => V::A(comps.into_boxed_slice()),
    }
}

/// Lanes of a scalar/vector as f32 (bools as 0/1).
fn lanes_f32(v: &V) -> SmallVec<[f32; 4]> {
    match v {
        V::F(x) => SmallVec::from_slice(&[*x]),
        V::I(x) => SmallVec::from_slice(&[*x as f32]),
        V::U(x) => SmallVec::from_slice(&[*x as f32]),
        V::B(x) => SmallVec::from_slice(&[*x as u8 as f32]),
        V::Fv(a, n) => a[..*n as usize].iter().copied().collect(),
        V::Iv(a, n) => a[..*n as usize].iter().map(|x| *x as f32).collect(),
        V::Uv(a, n) => a[..*n as usize].iter().map(|x| *x as f32).collect(),
        V::Bv(a, n) => a[..*n as usize].iter().map(|x| *x as u8 as f32).collect(),
        _ => SmallVec::new(),
    }
}

fn splat(v: V, n: u8) -> V {
    match v {
        V::F(x) => V::Fv([x; 4], n),
        V::I(x) => V::Iv([x; 4], n),
        V::U(x) => V::Uv([x; 4], n),
        V::B(x) => V::Bv([x; 4], n),
        o => o,
    }
}

fn swizzle(v: &V, n: u8, p: &[naga::SwizzleComponent; 4]) -> V {
    let idx = |i: usize| p[i] as usize;
    match v {
        V::Fv(a, _) => V::Fv([a[idx(0)], a[idx(1)], a[idx(2)], a[idx(3)]], n),
        V::Iv(a, _) => V::Iv([a[idx(0)], a[idx(1)], a[idx(2)], a[idx(3)]], n),
        V::Uv(a, _) => V::Uv([a[idx(0)], a[idx(1)], a[idx(2)], a[idx(3)]], n),
        V::Bv(a, _) => V::Bv([a[idx(0)], a[idx(1)], a[idx(2)], a[idx(3)]], n),
        V::F(x) => V::Fv([*x; 4], n),
        o => o.clone(),
    }
}

fn index_value(v: &V, i: usize) -> V {
    match v {
        V::Fv(a, n) => V::F(a[i.min(*n as usize - 1)]),
        V::Iv(a, n) => V::I(a[i.min(*n as usize - 1)]),
        V::Uv(a, n) => V::U(a[i.min(*n as usize - 1)]),
        V::Bv(a, n) => V::B(a[i.min(*n as usize - 1)]),
        V::M(m, c, r) => {
            let col = i.min(*c as usize - 1);
            let mut o = [0.0; 4];
            o[..*r as usize].copy_from_slice(&m[col * 4..col * 4 + *r as usize]);
            V::Fv(o, *r)
        }
        V::A(items) => items
            .get(i.min(items.len().saturating_sub(1)))
            .cloned()
            .unwrap_or(V::Void),
        o => o.clone(),
    }
}

/// Mutable reference to element `i` of a composite.
fn index_mut(v: &mut V, i: usize) -> Option<&mut V> {
    match v {
        V::A(items) => {
            let n = items.len();
            if n == 0 {
                return None;
            }
            items.get_mut(i.min(n - 1))
        }
        _ => None,
    }
}

/// Writes `x` into component/column `i` of a vector/matrix.
fn set_component(v: &mut V, i: usize, x: &V) {
    match v {
        V::Fv(a, n) if i < *n as usize => a[i] = x.as_f32(),
        V::Iv(a, n) if i < *n as usize => a[i] = x.as_i32(),
        V::Uv(a, n) if i < *n as usize => a[i] = x.as_i32() as u32,
        V::Bv(a, n) if i < *n as usize => a[i] = x.as_bool(),
        V::M(m, c, r) if i < *c as usize => {
            let lanes = lanes_f32(x);
            for (row, l) in lanes.iter().take(*r as usize).enumerate() {
                m[i * 4 + row] = *l;
            }
        }
        _ => {}
    }
}

fn map_f(v: &V, f: impl Fn(f32) -> f32) -> V {
    match v {
        V::F(x) => V::F(f(*x)),
        V::Fv(a, n) => {
            let mut o = *a;
            for x in o.iter_mut().take(*n as usize) {
                *x = f(*x);
            }
            V::Fv(o, *n)
        }
        V::M(m, c, r) => {
            let mut o = *m;
            for x in o.iter_mut() {
                *x = f(*x);
            }
            V::M(o, *c, *r)
        }
        o => o.clone(),
    }
}

fn map2_f(a: &V, b: &V, f: impl Fn(f32, f32) -> f32) -> V {
    match (a.fl(), b.fl()) {
        (Some((x, na)), Some((y, nb))) => {
            let n = na.max(nb);
            let mut o = [0.0; 4];
            for i in 0..(n.max(1) as usize) {
                let xa = if na == 0 { x[0] } else { x[i] };
                let yb = if nb == 0 { y[0] } else { y[i] };
                o[i] = f(xa, yb);
            }
            fv(o, n)
        }
        _ => V::Void,
    }
}

fn map3_f(a: &V, b: &V, c: &V, f: impl Fn(f32, f32, f32) -> f32) -> V {
    match (a.fl(), b.fl(), c.fl()) {
        (Some((x, na)), Some((y, nb)), Some((z, nc))) => {
            let n = na.max(nb).max(nc);
            let mut o = [0.0; 4];
            for i in 0..(n.max(1) as usize) {
                let pick = |v: &[f32; 4], k: u8| if k == 0 { v[0] } else { v[i] };
                o[i] = f(pick(&x, na), pick(&y, nb), pick(&z, nc));
            }
            fv(o, n)
        }
        _ => V::Void,
    }
}

/// Int lanes (`n` = 0: scalar).
fn il(v: &V) -> Option<([i32; 4], u8, bool)> {
    Some(match v {
        V::I(x) => ([*x; 4], 0, false),
        V::Iv(a, n) => (*a, *n, false),
        V::U(x) => ([*x as i32; 4], 0, true),
        V::Uv(a, n) => (a.map(|x| x as i32), *n, true),
        _ => return None,
    })
}

fn map2_i(a: &V, b: &V, f: impl Fn(i32, i32) -> i32) -> V {
    match (il(a), il(b)) {
        (Some((x, na, ua)), Some((y, nb, _))) => {
            let n = na.max(nb);
            let mut o = [0; 4];
            for i in 0..(n.max(1) as usize) {
                let xa = if na == 0 { x[0] } else { x[i] };
                let yb = if nb == 0 { y[0] } else { y[i] };
                o[i] = f(xa, yb);
            }
            if ua {
                if n == 0 {
                    V::U(o[0] as u32)
                } else {
                    V::Uv(o.map(|x| x as u32), n)
                }
            } else if n == 0 {
                V::I(o[0])
            } else {
                V::Iv(o, n)
            }
        }
        _ => V::Void,
    }
}

fn cmp(a: &V, b: &V, f: impl Fn(f32, f32) -> bool) -> V {
    let la = lanes_f32(a);
    let lb = lanes_f32(b);
    let n = la.len().max(lb.len());
    if n <= 1 {
        let x = la.first().copied().unwrap_or(0.0);
        let y = lb.first().copied().unwrap_or(0.0);
        if let (V::I(p), V::I(q)) = (a, b) {
            // Integers compare exactly.
            return V::B(match p.cmp(q) {
                std::cmp::Ordering::Less => f(0.0, 1.0),
                std::cmp::Ordering::Equal => f(0.0, 0.0),
                std::cmp::Ordering::Greater => f(1.0, 0.0),
            });
        }
        return V::B(f(x, y));
    }
    let mut o = [false; 4];
    for (i, slot) in o.iter_mut().enumerate().take(n) {
        let x = if la.len() == 1 {
            la[0]
        } else {
            la.get(i).copied().unwrap_or(0.0)
        };
        let y = if lb.len() == 1 {
            lb[0]
        } else {
            lb.get(i).copied().unwrap_or(0.0)
        };
        *slot = f(x, y);
    }
    V::Bv(o, n as u8)
}

fn mat_mul_vec(m: &[f32; 16], c: u8, r: u8, v: &[f32; 4]) -> V {
    let mut o = [0.0; 4];
    for (row, out) in o.iter_mut().enumerate().take(r as usize) {
        let mut s = 0.0;
        for col in 0..c as usize {
            s += m[col * 4 + row] * v[col];
        }
        *out = s;
    }
    V::Fv(o, r)
}

fn vec_mul_mat(v: &[f32; 4], m: &[f32; 16], c: u8, r: u8) -> V {
    let mut o = [0.0; 4];
    for (col, out) in o.iter_mut().enumerate().take(c as usize) {
        let mut s = 0.0;
        for row in 0..r as usize {
            s += v[row] * m[col * 4 + row];
        }
        *out = s;
    }
    V::Fv(o, c)
}

pub fn binary(op: Bo, a: V, b: V) -> V {
    // Matrix products.
    if op == Bo::Multiply {
        match (&a, &b) {
            (V::M(m, c, r), V::Fv(v, _)) => return mat_mul_vec(m, *c, *r, v),
            (V::Fv(v, _), V::M(m, c, r)) => return vec_mul_mat(v, m, *c, *r),
            (V::M(x, xc, xr), V::M(y, yc, _yr)) => {
                let mut o = [0.0; 16];
                for col in 0..*yc as usize {
                    for row in 0..*xr as usize {
                        let mut s = 0.0;
                        for k in 0..*xc as usize {
                            s += x[k * 4 + row] * y[col * 4 + k];
                        }
                        o[col * 4 + row] = s;
                    }
                }
                return V::M(o, *yc, *xr);
            }
            (V::M(m, c, r), V::F(s)) | (V::F(s), V::M(m, c, r)) => {
                return V::M(m.map(|x| x * s), *c, *r);
            }
            _ => {}
        }
    }
    if let (V::M(x, c, r), V::M(y, _, _)) = (&a, &b) {
        let mut o = [0.0; 16];
        for i in 0..16 {
            o[i] = match op {
                Bo::Add => x[i] + y[i],
                Bo::Subtract => x[i] - y[i],
                Bo::Divide => x[i] / y[i],
                _ => x[i],
            };
        }
        return V::M(o, *c, *r);
    }
    let float = matches!(a, V::F(_) | V::Fv(..)) || matches!(b, V::F(_) | V::Fv(..));
    match op {
        Bo::Add | Bo::Subtract | Bo::Multiply | Bo::Divide | Bo::Modulo if float => {
            map2_f(&a, &b, |x, y| match op {
                Bo::Add => x + y,
                Bo::Subtract => x - y,
                Bo::Multiply => x * y,
                Bo::Divide => x / y,
                _ => x % y,
            })
        }
        Bo::Add => map2_i(&a, &b, i32::wrapping_add),
        Bo::Subtract => map2_i(&a, &b, i32::wrapping_sub),
        Bo::Multiply => map2_i(&a, &b, i32::wrapping_mul),
        Bo::Divide => map2_i(&a, &b, |x, y| if y == 0 { 0 } else { x.wrapping_div(y) }),
        Bo::Modulo => map2_i(&a, &b, |x, y| if y == 0 { 0 } else { x.wrapping_rem(y) }),
        Bo::Equal => cmp(&a, &b, |x, y| x == y),
        Bo::NotEqual => cmp(&a, &b, |x, y| x != y),
        Bo::Less => cmp(&a, &b, |x, y| x < y),
        Bo::LessEqual => cmp(&a, &b, |x, y| x <= y),
        Bo::Greater => cmp(&a, &b, |x, y| x > y),
        Bo::GreaterEqual => cmp(&a, &b, |x, y| x >= y),
        Bo::LogicalAnd | Bo::And if matches!(a, V::B(_) | V::Bv(..)) => {
            logic(&a, &b, |x, y| x && y)
        }
        Bo::LogicalOr | Bo::InclusiveOr if matches!(a, V::B(_) | V::Bv(..)) => {
            logic(&a, &b, |x, y| x || y)
        }
        Bo::ExclusiveOr if matches!(a, V::B(_) | V::Bv(..)) => logic(&a, &b, |x, y| x != y),
        Bo::And => map2_i(&a, &b, |x, y| x & y),
        Bo::InclusiveOr => map2_i(&a, &b, |x, y| x | y),
        Bo::ExclusiveOr => map2_i(&a, &b, |x, y| x ^ y),
        Bo::ShiftLeft => map2_i(&a, &b, |x, y| x.wrapping_shl(y as u32)),
        Bo::ShiftRight => map2_i(&a, &b, |x, y| x.wrapping_shr(y as u32)),
        Bo::LogicalAnd => V::B(a.as_bool() && b.as_bool()),
        Bo::LogicalOr => V::B(a.as_bool() || b.as_bool()),
    }
}

fn logic(a: &V, b: &V, f: impl Fn(bool, bool) -> bool) -> V {
    match (a, b) {
        (V::Bv(x, n), V::Bv(y, _)) => {
            let mut o = [false; 4];
            for i in 0..*n as usize {
                o[i] = f(x[i], y[i]);
            }
            V::Bv(o, *n)
        }
        _ => V::B(f(a.as_bool(), b.as_bool())),
    }
}

pub fn unary(op: UnaryOperator, v: V) -> V {
    match op {
        UnaryOperator::Negate => match v {
            V::I(x) => V::I(x.wrapping_neg()),
            V::Iv(a, n) => V::Iv(a.map(i32::wrapping_neg), n),
            o => map_f(&o, |x| -x),
        },
        UnaryOperator::LogicalNot => match v {
            V::Bv(a, n) => V::Bv(a.map(|x| !x), n),
            o => V::B(!o.as_bool()),
        },
        UnaryOperator::BitwiseNot => match v {
            V::I(x) => V::I(!x),
            V::U(x) => V::U(!x),
            V::Iv(a, n) => V::Iv(a.map(|x| !x), n),
            V::Uv(a, n) => V::Uv(a.map(|x| !x), n),
            V::B(b) => V::B(!b),
            o => o,
        },
    }
}

fn select(c: V, a: V, b: V) -> V {
    match c {
        V::Bv(m, n) => {
            let la = lanes_f32(&a);
            let lb = lanes_f32(&b);
            let mut o = [0.0; 4];
            for i in 0..n as usize {
                o[i] = if m[i] {
                    la.get(i).copied().unwrap_or(0.0)
                } else {
                    lb.get(i).copied().unwrap_or(0.0)
                };
            }
            match a {
                V::Iv(..) => V::Iv(o.map(|x| x as i32), n),
                V::Bv(..) => V::Bv(o.map(|x| x != 0.0), n),
                _ => V::Fv(o, n),
            }
        }
        c => {
            if c.as_bool() {
                a
            } else {
                b
            }
        }
    }
}

fn relational(f: RelationalFunction, v: V) -> V {
    match f {
        RelationalFunction::All => match v {
            V::Bv(a, n) => V::B(a[..n as usize].iter().all(|x| *x)),
            o => V::B(o.as_bool()),
        },
        RelationalFunction::Any => match v {
            V::Bv(a, n) => V::B(a[..n as usize].iter().any(|x| *x)),
            o => V::B(o.as_bool()),
        },
        RelationalFunction::IsNan | RelationalFunction::IsInf => {
            let test = |x: f32| {
                if f == RelationalFunction::IsNan {
                    x.is_nan()
                } else {
                    x.is_infinite()
                }
            };
            match v {
                V::F(x) => V::B(test(x)),
                V::Fv(a, n) => V::Bv(a.map(test), n),
                _ => V::B(false),
            }
        }
    }
}

fn dot(a: &V, b: &V) -> f32 {
    match (a, b) {
        (V::Fv(x, n), V::Fv(y, _)) => (0..*n as usize).map(|i| x[i] * y[i]).sum(),
        (V::F(x), V::F(y)) => x * y,
        _ => lanes_f32(a)
            .iter()
            .zip(lanes_f32(b).iter())
            .map(|(x, y)| x * y)
            .sum(),
    }
}

fn length(v: &V) -> f32 {
    dot(v, v).sqrt()
}

fn glsl_round(x: f32) -> f32 {
    // Round half to even like most GPUs.
    let r = x.round();
    if (x - x.trunc()).abs() == 0.5 {
        2.0 * (x / 2.0).round()
    } else {
        r
    }
}

pub fn math(f: Mf, a: V, b: Option<V>, c: Option<V>) -> Result<V, EvalError> {
    let b_ = || b.clone().unwrap_or(V::F(0.0));
    let c_ = || c.clone().unwrap_or(V::F(0.0));
    let is_int = matches!(a, V::I(_) | V::Iv(..) | V::U(_) | V::Uv(..));
    Ok(match f {
        Mf::Abs if is_int => match a {
            V::I(x) => V::I(x.wrapping_abs()),
            V::Iv(v, n) => V::Iv(v.map(i32::wrapping_abs), n),
            o => o,
        },
        Mf::Abs => map_f(&a, f32::abs),
        Mf::Min if is_int => map2_i(&a, &b_(), i32::min),
        Mf::Max if is_int => map2_i(&a, &b_(), i32::max),
        Mf::Min => map2_f(&a, &b_(), |x, y| if y < x { y } else { x }),
        Mf::Max => map2_f(&a, &b_(), |x, y| if y > x { y } else { x }),
        Mf::Clamp if is_int => {
            let lo = map2_i(&a, &b_(), i32::max);
            map2_i(&lo, &c_(), i32::min)
        }
        Mf::Clamp => map3_f(&a, &b_(), &c_(), |x, lo, hi| x.max(lo).min(hi)),
        Mf::Saturate => map_f(&a, |x| x.clamp(0.0, 1.0)),
        Mf::Cos => map_f(&a, f32::cos),
        Mf::Cosh => map_f(&a, f32::cosh),
        Mf::Sin => map_f(&a, f32::sin),
        Mf::Sinh => map_f(&a, f32::sinh),
        Mf::Tan => map_f(&a, f32::tan),
        Mf::Tanh => map_f(&a, f32::tanh),
        Mf::Acos => map_f(&a, f32::acos),
        Mf::Asin => map_f(&a, f32::asin),
        Mf::Atan => map_f(&a, f32::atan),
        Mf::Atan2 => map2_f(&a, &b_(), f32::atan2),
        Mf::Asinh => map_f(&a, f32::asinh),
        Mf::Acosh => map_f(&a, f32::acosh),
        Mf::Atanh => map_f(&a, f32::atanh),
        Mf::Radians => map_f(&a, f32::to_radians),
        Mf::Degrees => map_f(&a, f32::to_degrees),
        Mf::Ceil => map_f(&a, f32::ceil),
        Mf::Floor => map_f(&a, f32::floor),
        Mf::Round => map_f(&a, glsl_round),
        Mf::Fract => map_f(&a, |x| x - x.floor()),
        Mf::Trunc => map_f(&a, f32::trunc),
        Mf::Ldexp => map2_f(&a, &b_(), |x, e| x * (e as i32 as f32).exp2()),
        Mf::Exp => map_f(&a, f32::exp),
        Mf::Exp2 => map_f(&a, f32::exp2),
        Mf::Log => map_f(&a, f32::ln),
        Mf::Log2 => map_f(&a, f32::log2),
        Mf::Pow => map2_f(&a, &b_(), f32::powf),
        Mf::Dot => V::F(dot(&a, &b_())),
        Mf::Cross => {
            let (x, y) = (a.vec4(), b_().vec4());
            V::Fv(
                [
                    x[1] * y[2] - x[2] * y[1],
                    x[2] * y[0] - x[0] * y[2],
                    x[0] * y[1] - x[1] * y[0],
                    0.0,
                ],
                3,
            )
        }
        Mf::Distance => {
            let d = map2_f(&a, &b_(), |x, y| x - y);
            V::F(length(&d))
        }
        Mf::Length => V::F(length(&a)),
        Mf::Normalize => {
            let l = length(&a);
            map_f(&a, |x| x / l)
        }
        Mf::FaceForward => {
            // faceforward(N, I, Nref) = dot(Nref, I) < 0 ? N : -N
            if dot(&c_(), &b_()) < 0.0 {
                a
            } else {
                map_f(&a, |x| -x)
            }
        }
        Mf::Reflect => {
            // I - 2 * dot(N, I) * N
            let n = b_();
            let d = dot(&n, &a);
            map2_f(&a, &n, |i, n| i - 2.0 * d * n)
        }
        Mf::Refract => {
            let n = b_();
            let eta = c_().as_f32();
            let d = dot(&n, &a);
            let k = 1.0 - eta * eta * (1.0 - d * d);
            if k < 0.0 {
                map_f(&a, |_| 0.0)
            } else {
                let s = eta * d + k.sqrt();
                map2_f(&a, &n, |i, n| eta * i - s * n)
            }
        }
        Mf::Sign if is_int => match a {
            V::I(x) => V::I(x.signum()),
            V::Iv(v, n) => V::Iv(v.map(i32::signum), n),
            o => o,
        },
        Mf::Sign => map_f(&a, |x| {
            if x > 0.0 {
                1.0
            } else if x < 0.0 {
                -1.0
            } else {
                0.0
            }
        }),
        Mf::Fma => map3_f(&a, &b_(), &c_(), |x, y, z| x * y + z),
        Mf::Mix => match c_() {
            V::Bv(m, n) => {
                let (x, y) = (a.vec4(), b_().vec4());
                let mut o = [0.0; 4];
                for i in 0..n as usize {
                    o[i] = if m[i] { y[i] } else { x[i] };
                }
                V::Fv(o, n)
            }
            V::B(m) => {
                if m {
                    b_()
                } else {
                    a
                }
            }
            t => map3_f(&a, &b_(), &t, |x, y, t| x + (y - x) * t),
        },
        Mf::Step => map2_f(&a, &b_(), |edge, x| if x < edge { 0.0 } else { 1.0 }),
        Mf::SmoothStep => map3_f(&a, &b_(), &c_(), |e0, e1, x| {
            let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
            t * t * (3.0 - 2.0 * t)
        }),
        Mf::Sqrt => map_f(&a, f32::sqrt),
        Mf::InverseSqrt => map_f(&a, |x| 1.0 / x.sqrt()),
        Mf::Transpose => match a {
            V::M(m, c, r) => {
                let mut o = [0.0; 16];
                for col in 0..c as usize {
                    for row in 0..r as usize {
                        o[row * 4 + col] = m[col * 4 + row];
                    }
                }
                V::M(o, r, c)
            }
            o => o,
        },
        Mf::Determinant => match a {
            V::M(m, 2, 2) => V::F(m[0] * m[5] - m[4] * m[1]),
            V::M(m, 3, 3) => V::F(
                m[0] * (m[5] * m[10] - m[9] * m[6]) - m[4] * (m[1] * m[10] - m[9] * m[2])
                    + m[8] * (m[1] * m[6] - m[5] * m[2]),
            ),
            _ => V::F(0.0),
        },
        Mf::Inverse => match a {
            V::M(m, 2, 2) => {
                let d = m[0] * m[5] - m[4] * m[1];
                let mut o = [0.0; 16];
                o[0] = m[5] / d;
                o[1] = -m[1] / d;
                o[4] = -m[4] / d;
                o[5] = m[0] / d;
                V::M(o, 2, 2)
            }
            V::M(m, 3, 3) => {
                let e = |c: usize, r: usize| m[c * 4 + r];
                let det = e(0, 0) * (e(1, 1) * e(2, 2) - e(2, 1) * e(1, 2))
                    - e(1, 0) * (e(0, 1) * e(2, 2) - e(2, 1) * e(0, 2))
                    + e(2, 0) * (e(0, 1) * e(1, 2) - e(1, 1) * e(0, 2));
                let mut o = [0.0; 16];
                for c in 0..3 {
                    for r in 0..3 {
                        // inverse[c][r] = cofactor(r, c) / det
                        let (c1, c2) = ((r + 1) % 3, (r + 2) % 3);
                        let (r1, r2) = ((c + 1) % 3, (c + 2) % 3);
                        o[c * 4 + r] = (e(c1, r1) * e(c2, r2) - e(c2, r1) * e(c1, r2)) / det;
                    }
                }
                V::M(o, 3, 3)
            }
            o => o,
        },
        Mf::Outer => {
            let (x, y) = (lanes_f32(&a), lanes_f32(&b_()));
            let mut o = [0.0; 16];
            for (c, yv) in y.iter().enumerate() {
                for (r, xv) in x.iter().enumerate() {
                    o[c * 4 + r] = xv * yv;
                }
            }
            V::M(o, y.len() as u8, x.len() as u8)
        }
        Mf::CountOneBits => match a {
            V::I(x) => V::I(x.count_ones() as i32),
            V::U(x) => V::U(x.count_ones()),
            o => o,
        },
        other => return unsupported(&format!("built-in {other:?}")),
    })
}

/// `As` conversion (`convert`: numeric conversion; otherwise bitcast).
pub fn cast(v: V, kind: ScalarKind, convert: bool) -> V {
    let one = |x: &V| -> V {
        match (kind, convert) {
            (ScalarKind::Float | ScalarKind::AbstractFloat, true) => V::F(x.as_f32()),
            (ScalarKind::Sint | ScalarKind::AbstractInt, true) => V::I(match x {
                V::F(f) => *f as i32,
                o => o.as_i32(),
            }),
            (ScalarKind::Uint, true) => V::U(match x {
                V::F(f) => *f as u32,
                o => o.as_i32() as u32,
            }),
            (ScalarKind::Bool, _) => V::B(x.as_bool()),
            (ScalarKind::Float | ScalarKind::AbstractFloat, false) => V::F(match x {
                V::I(i) => f32::from_bits(*i as u32),
                V::U(u) => f32::from_bits(*u),
                o => o.as_f32(),
            }),
            (ScalarKind::Sint | ScalarKind::AbstractInt, false) => V::I(match x {
                V::F(f) => f.to_bits() as i32,
                o => o.as_i32(),
            }),
            (ScalarKind::Uint, false) => V::U(match x {
                V::F(f) => f.to_bits(),
                o => o.as_i32() as u32,
            }),
        }
    };
    let scalar = |x: f32, src: &V| -> V {
        let s = match src {
            V::Iv(..) => V::I(x as i32),
            V::Uv(..) => V::U(x as u32),
            V::Bv(..) => V::B(x != 0.0),
            _ => V::F(x),
        };
        one(&s)
    };
    match &v {
        V::Fv(_, n) | V::Iv(_, n) | V::Uv(_, n) | V::Bv(_, n) => {
            let n = *n;
            let lanes: SmallVec<[V; 4]> = match &v {
                V::Fv(a, _) => a[..n as usize].iter().map(|x| one(&V::F(*x))).collect(),
                V::Iv(a, _) => a[..n as usize].iter().map(|x| one(&V::I(*x))).collect(),
                V::Uv(a, _) => a[..n as usize].iter().map(|x| one(&V::U(*x))).collect(),
                V::Bv(a, _) => a[..n as usize].iter().map(|x| one(&V::B(*x))).collect(),
                _ => SmallVec::new(),
            };
            let _ = scalar;
            match kind {
                ScalarKind::Float | ScalarKind::AbstractFloat => {
                    let mut o = [0.0; 4];
                    for (i, l) in lanes.iter().enumerate() {
                        o[i] = l.as_f32();
                    }
                    V::Fv(o, n)
                }
                ScalarKind::Sint | ScalarKind::AbstractInt => {
                    let mut o = [0; 4];
                    for (i, l) in lanes.iter().enumerate() {
                        o[i] = l.as_i32();
                    }
                    V::Iv(o, n)
                }
                ScalarKind::Uint => {
                    let mut o = [0; 4];
                    for (i, l) in lanes.iter().enumerate() {
                        o[i] = match l {
                            V::U(u) => *u,
                            o => o.as_i32() as u32,
                        };
                    }
                    V::Uv(o, n)
                }
                ScalarKind::Bool => {
                    let mut o = [false; 4];
                    for (i, l) in lanes.iter().enumerate() {
                        o[i] = l.as_bool();
                    }
                    V::Bv(o, n)
                }
            }
        }
        V::M(..) => v,
        x => one(x),
    }
}

/// Per-thread interpreter state.
pub struct Exec<'p> {
    p: &'p Program,
    tex: &'p dyn Textures,
    init: &'p [V],
    pub globals: Vec<V>,
    frames: Vec<Frame>,
    pool: Vec<Frame>,
    iterations: u64,
    pub error: Option<EvalError>,
}

impl<'p> Exec<'p> {
    pub fn new(p: &'p Program, tex: &'p dyn Textures) -> Self {
        Self::with_globals(p, tex, &p.globals_init)
    }

    /// Like [`Self::new`] with other initial global values (bound uniforms); `init` must
    /// come from [`Program::globals_init`].
    pub fn with_globals(p: &'p Program, tex: &'p dyn Textures, init: &'p [V]) -> Self {
        Self {
            p,
            tex,
            init,
            globals: init.to_vec(),
            frames: Vec::new(),
            pool: Vec::new(),
            iterations: 0,
            error: None,
        }
    }

    /// Runs `main` for one fragment; returns `gl_FragColor` (`None` = discarded).
    pub fn run(&mut self, frag_coord: [f32; 4]) -> Result<Option<[f32; 4]>, EvalError> {
        self.globals.clone_from_slice(self.init);
        if let Some(i) = self.p.frag_coord {
            self.globals[i as usize] = V::Fv(frag_coord, 4);
        }
        self.iterations = 0;
        self.frames.clear();
        let f = &self.p.module.entry_points[0].function;
        let killed = matches!(self.call(f, Vec::new())?, Flow::Kill);
        if killed {
            return Ok(None);
        }
        Ok(Some(match self.p.frag_color {
            Some(i) => self.globals[i as usize].vec4(),
            None => [0.0; 4],
        }))
    }

    fn call(&mut self, f: &'p Function, args: Vec<V>) -> Result<Flow, EvalError> {
        if self.frames.len() > 64 {
            return unsupported("recursion");
        }
        let mut fr = self.pool.pop().unwrap_or(Frame {
            vals: Vec::new(),
            locals: Vec::new(),
            args: Vec::new(),
        });
        fr.vals.clear();
        fr.vals.resize(f.expressions.len(), None);
        fr.locals.clear();
        fr.args = args;
        self.frames.push(fr);
        // Locals: zero, then initializers (constant expressions of this function).
        for (_, l) in f.local_variables.iter() {
            let z = zero(&self.p.module, l.ty);
            self.top().locals.push(z);
        }
        for (i, (_, l)) in f.local_variables.iter().enumerate() {
            if let Some(init) = l.init {
                let v = self.get(f, init)?;
                self.top().locals[i] = v;
            }
        }
        let r = self.block(f, &f.body);
        if let Some(fr) = self.frames.pop() {
            self.pool.push(fr);
        }
        r
    }

    fn top(&mut self) -> &mut Frame {
        self.frames.last_mut().expect("frame")
    }

    fn get(&mut self, f: &'p Function, h: Handle<E>) -> Result<V, EvalError> {
        if let Some(v) = &self.frames.last().expect("frame").vals[h.index()] {
            return Ok(v.clone());
        }
        let v = self.eval(f, h)?;
        self.top().vals[h.index()] = Some(v.clone());
        Ok(v)
    }

    fn eval(&mut self, f: &'p Function, h: Handle<E>) -> Result<V, EvalError> {
        Ok(match &f.expressions[h] {
            E::Literal(l) => literal(l),
            E::Constant(c) => self.p.consts[c.index()].clone(),
            E::Override(_) => V::F(0.0),
            E::ZeroValue(t) => zero(&self.p.module, *t),
            E::Compose { ty, components } => {
                let mut vals = Vec::with_capacity(components.len());
                for c in components {
                    vals.push(self.get(f, *c)?);
                }
                compose(&self.p.module, *ty, vals)
            }
            E::Access { base, index } => {
                let i = self.get(f, *index)?.as_i32().max(0) as usize;
                match self.get(f, *base)? {
                    V::P(mut p) => {
                        p.path.push(i as u16);
                        V::P(p)
                    }
                    v => index_value(&v, i),
                }
            }
            E::AccessIndex { base, index } => match self.get(f, *base)? {
                V::P(mut p) => {
                    p.path.push(*index as u16);
                    V::P(p)
                }
                v => index_value(&v, *index as usize),
            },
            E::Splat { size, value } => splat(self.get(f, *value)?, *size as u8),
            E::Swizzle {
                size,
                vector,
                pattern,
            } => swizzle(&self.get(f, *vector)?, *size as u8, pattern),
            E::FunctionArgument(i) => self
                .frames
                .last()
                .expect("frame")
                .args
                .get(*i as usize)
                .cloned()
                .unwrap_or(V::Void),
            E::GlobalVariable(g) => V::P(Ptr {
                root: Root::Global(g.index() as u32),
                path: SmallVec::new(),
            }),
            E::LocalVariable(l) => V::P(Ptr {
                root: Root::Local(self.frames.len() as u32 - 1, l.index() as u32),
                path: SmallVec::new(),
            }),
            E::Load { pointer } => match self.get(f, *pointer)? {
                V::P(p) => self.load(&p),
                v => v,
            },
            E::Unary { op, expr } => unary(*op, self.get(f, *expr)?),
            E::Binary { op, left, right } => {
                let a = self.get(f, *left)?;
                let b = self.get(f, *right)?;
                binary(*op, a, b)
            }
            E::Select {
                condition,
                accept,
                reject,
            } => {
                let c = self.get(f, *condition)?;
                let a = self.get(f, *accept)?;
                let b = self.get(f, *reject)?;
                select(c, a, b)
            }
            E::Derivative { expr, .. } => {
                let v = self.get(f, *expr)?;
                map_f(&v, |_| 0.0)
            }
            E::Relational { fun, argument } => relational(*fun, self.get(f, *argument)?),
            E::Math {
                fun,
                arg,
                arg1,
                arg2,
                arg3: _,
            } => {
                let a = self.get(f, *arg)?;
                let b = match arg1 {
                    Some(x) => Some(self.get(f, *x)?),
                    None => None,
                };
                let c = match arg2 {
                    Some(x) => Some(self.get(f, *x)?),
                    None => None,
                };
                math(*fun, a, b, c)?
            }
            E::As {
                expr,
                kind,
                convert,
            } => cast(self.get(f, *expr)?, *kind, convert.is_some()),
            E::CallResult(_) => V::Void,
            other => return unsupported(&format!("expression {other:?}")),
        })
    }

    fn storage(&mut self, root: Root) -> Option<&mut V> {
        match root {
            Root::Global(i) => self.globals.get_mut(i as usize),
            Root::Local(d, i) => self.frames.get_mut(d as usize)?.locals.get_mut(i as usize),
        }
    }

    fn load(&mut self, p: &Ptr) -> V {
        let Some(mut v) = self.storage(p.root).map(|v| &*v) else {
            return V::Void;
        };
        for (k, &i) in p.path.iter().enumerate() {
            match v {
                V::A(items) => {
                    let n = items.len();
                    if n == 0 {
                        return V::Void;
                    }
                    v = &items[(i as usize).min(n - 1)];
                }
                other => {
                    // Vector component / matrix column (then component).
                    let x = index_value(other, i as usize);
                    return match p.path.get(k + 1) {
                        Some(j) => index_value(&x, *j as usize),
                        None => x,
                    };
                }
            }
        }
        v.clone()
    }

    fn store(&mut self, p: &Ptr, x: V) {
        let Some(mut v) = self.storage(p.root) else {
            return;
        };
        let n = p.path.len();
        for (k, &i) in p.path.iter().enumerate() {
            if matches!(v, V::A(_)) {
                let Some(next) = index_mut(v, i as usize) else {
                    return;
                };
                v = next;
                continue;
            }
            if k + 1 == n {
                set_component(v, i as usize, &x);
            } else if let (V::M(m, _, r), Some(j)) = (&mut *v, p.path.get(k + 1)) {
                // Matrix element m[i][j].
                if (*j as u8) < *r {
                    m[i as usize * 4 + *j as usize] = x.as_f32();
                }
            }
            return;
        }
        *v = x;
    }

    fn block(&mut self, f: &'p Function, b: &'p naga::Block) -> Result<Flow, EvalError> {
        for s in b.iter() {
            match self.stmt(f, s)? {
                Flow::Next => {}
                other => return Ok(other),
            }
        }
        Ok(Flow::Next)
    }

    fn stmt(&mut self, f: &'p Function, s: &'p S) -> Result<Flow, EvalError> {
        match s {
            S::Emit(range) => {
                for h in range.clone() {
                    let v = self.eval(f, h)?;
                    self.top().vals[h.index()] = Some(v);
                }
            }
            S::Block(b) => return self.block(f, b),
            S::If {
                condition,
                accept,
                reject,
            } => {
                let c = self.get(f, *condition)?.as_bool();
                return self.block(f, if c { accept } else { reject });
            }
            S::Switch { selector, cases } => {
                let sel = self.get(f, *selector)?.as_i32();
                let mut matched = false;
                for c in cases {
                    if !matched {
                        matched = match c.value {
                            naga::SwitchValue::I32(v) => v == sel,
                            naga::SwitchValue::U32(v) => v as i32 == sel,
                            naga::SwitchValue::Default => true,
                        };
                    }
                    if matched {
                        match self.block(f, &c.body)? {
                            Flow::Break => return Ok(Flow::Next),
                            Flow::Next if c.fall_through => continue,
                            Flow::Next => return Ok(Flow::Next),
                            other => return Ok(other),
                        }
                    }
                }
                // No case matched: run the default even if it is not last.
                if !matched
                    && let Some(c) = cases.iter().find(|c| c.value == naga::SwitchValue::Default)
                {
                    return match self.block(f, &c.body)? {
                        Flow::Break | Flow::Next => Ok(Flow::Next),
                        other => Ok(other),
                    };
                }
            }
            S::Loop {
                body,
                continuing,
                break_if,
            } => loop {
                self.iterations += 1;
                if self.iterations > MAX_ITERATIONS {
                    return Err(EvalError::TooLong);
                }
                match self.block(f, body)? {
                    Flow::Break => break,
                    Flow::Return(v) => return Ok(Flow::Return(v)),
                    Flow::Kill => return Ok(Flow::Kill),
                    Flow::Next | Flow::Continue => {}
                }
                match self.block(f, continuing)? {
                    Flow::Break => break,
                    Flow::Return(v) => return Ok(Flow::Return(v)),
                    Flow::Kill => return Ok(Flow::Kill),
                    _ => {}
                }
                if let Some(c) = break_if
                    && self.get(f, *c)?.as_bool()
                {
                    break;
                }
            },
            S::Break => return Ok(Flow::Break),
            S::Continue => return Ok(Flow::Continue),
            S::Return { value } => {
                let v = match value {
                    Some(v) => self.get(f, *v)?,
                    None => V::Void,
                };
                return Ok(Flow::Return(v));
            }
            S::Kill => return Ok(Flow::Kill),
            S::Store { pointer, value } => {
                let v = self.get(f, *value)?;
                if let V::P(p) = self.get(f, *pointer)? {
                    self.store(&p, v);
                }
            }
            S::Call {
                function,
                arguments,
                result,
            } => {
                let mut args = Vec::with_capacity(arguments.len());
                for a in arguments {
                    args.push(self.get(f, *a)?);
                }
                let callee = &self.p.module.functions[*function];
                let v = match self.p.host[function.index()] {
                    Host::Tex => {
                        let uv = args.get(1).map_or([0.0; 4], V::vec4);
                        let bias = args.get(2).map_or(0.0, V::as_f32);
                        let slot = args.first().map_or(-1, V::as_i32);
                        V::Fv(self.tex.sample(slot, [uv[0], uv[1]], bias.max(0.0)), 4)
                    }
                    Host::TexLod => {
                        let uv = args.get(1).map_or([0.0; 4], V::vec4);
                        let lod = args.get(2).map_or(0.0, V::as_f32);
                        let slot = args.first().map_or(-1, V::as_i32);
                        V::Fv(self.tex.sample(slot, [uv[0], uv[1]], lod.max(0.0)), 4)
                    }
                    Host::TexSize => {
                        let slot = args.first().map_or(-1, V::as_i32);
                        let s = self.tex.size(slot);
                        V::Iv([s[0], s[1], 0, 0], 2)
                    }
                    Host::None => match self.call(callee, args)? {
                        Flow::Return(v) => v,
                        Flow::Kill => return Ok(Flow::Kill),
                        _ => V::Void,
                    },
                };
                if let Some(r) = result {
                    self.top().vals[r.index()] = Some(v);
                }
            }
            S::ControlBarrier(_) | S::MemoryBarrier(_) => {}
            other => return unsupported(&format!("statement {other:?}")),
        }
        Ok(Flow::Next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::glsl::{parse, rewrite};

    fn run(src: &str, coord: [f32; 2]) -> [f32; 4] {
        let rw = rewrite(src);
        let m = parse(&rw).unwrap();
        let p = Program::new(m).unwrap();
        let mut ex = Exec::new(&p, &NoTextures);
        ex.run([coord[0], coord[1], 0.0, 1.0]).unwrap().unwrap()
    }

    #[test]
    fn arithmetic_and_control_flow() {
        let c = run(
            "float sq(float x) { return x * x; }\n\
             void main() {\n\
               vec2 p = gl_FragCoord.xy;\n\
               float s = 0.0;\n\
               for (int i = 0; i < 4; i++) { if (i == 2) continue; s += float(i); }\n\
               vec3 v = vec3(1.0, 2.0, 3.0);\n\
               v.yz = v.zy;\n\
               mat2 r = mat2(0.0, 1.0, -1.0, 0.0);\n\
               vec2 q = r * vec2(1.0, 0.0);\n\
               gl_FragColor = vec4(sq(p.x), s, v.y + q.y, mod(-1.0, 3.0));\n\
             }",
            [3.0, 0.0],
        );
        assert_eq!(c, [9.0, 4.0, 4.0, 2.0]);
    }

    #[test]
    fn arrays_structs_and_out_params() {
        let c = run(
            "struct S { float a; vec2 b; };\n\
             void f(inout float x, out vec2 y) { x += 1.0; y = vec2(5.0, 6.0); }\n\
             float g_arr[3];\n\
             void main() {\n\
               g_arr[1] = 2.0;\n\
               S s = S(1.0, vec2(0.0));\n\
               f(s.a, s.b);\n\
               int k = 1;\n\
               gl_FragColor = vec4(s.a, s.b.y, g_arr[k], float(ivec2(3, 4).y));\n\
             }",
            [0.0, 0.0],
        );
        assert_eq!(c, [2.0, 6.0, 2.0, 4.0]);
    }

    #[test]
    fn builtins() {
        let c = run(
            "void main() {\n\
               float a = clamp(smoothstep(0.0, 1.0, 0.5), 0.0, 1.0);\n\
               float b = length(vec2(3.0, 4.0));\n\
               float d = dot(normalize(vec3(0.0, 0.0, 2.0)), vec3(0.0, 0.0, 1.0));\n\
               float e = step(0.5, 0.4) + fract(1.25);\n\
               gl_FragColor = vec4(a, b, d, e);\n\
             }",
            [0.0, 0.0],
        );
        assert_eq!(c, [0.5, 5.0, 1.0, 0.25]);
    }

    #[test]
    fn discard_and_infinite_loops() {
        let rw =
            rewrite("void main() { if (gl_FragCoord.x > 1.0) discard; gl_FragColor = vec4(1.0); }");
        let p = Program::new(parse(&rw).unwrap()).unwrap();
        let mut ex = Exec::new(&p, &NoTextures);
        assert_eq!(ex.run([2.0, 0.0, 0.0, 1.0]).unwrap(), None);
        assert_eq!(ex.run([0.0, 0.0, 0.0, 1.0]).unwrap(), Some([1.0; 4]));
        let rw = rewrite(
            "void main() { float x = 0.0; for (int i = 0; i >= 0; i++) { x += 1.0; } gl_FragColor = vec4(x); }",
        );
        let p = Program::new(parse(&rw).unwrap()).unwrap();
        let mut ex = Exec::new(&p, &NoTextures);
        assert_eq!(ex.run([0.0; 4]), Err(EvalError::TooLong));
    }
}
