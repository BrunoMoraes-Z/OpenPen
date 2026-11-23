//! GLSL ES 1.0 (WebGL 1) fragment shaders -> naga IR.
//!
//! The `.pen` shader dialect is a WebGL 1.0 fragment shader (`gl_FragCoord`,
//! `gl_FragColor`, `texture2D`, plus `textureSize(sampler, lod)`) whose uniforms carry
//! JSDoc-like annotations (`@label`, `@default`, `@range`, `@color`, `@resolution`,
//! `@time`, `@mouse`, `@sdf`, `@mipmap`). naga's GLSL frontend only accepts Vulkan
//! GLSL 4.40+, so the source is rewritten token by token first:
//!
//! - `#version` / `#extension` / `precision ...;` and precision qualifiers are dropped;
//! - every `uniform` becomes a plain (private) global that the evaluator fills before
//!   running `main`; `sampler2D` becomes `int` (a texture slot), so samplers can also be
//!   passed to user functions;
//! - `texture2D` / `texture2DLod` / `textureSize` call host functions (`_pen_tex*`),
//!   whose bodies are stubs: the evaluator intercepts them;
//! - `gl_FragCoord` / `gl_FragColor` are plain globals;
//! - identifiers that are keywords or built-ins in GLSL 4.50 but not in ES 1.0 (`sample`,
//!   `filter`, user-defined `round`/`tanh`/...) are renamed.
//!
//! Line numbers are kept (the prelude takes two lines, subtracted from diagnostics).

use std::collections::HashSet;

/// Special role of a uniform (auto-bound, not taken from `uniforms`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    None,
    Resolution,
    Time,
    Mouse,
    Sdf,
}

/// Declared type of a uniform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UType {
    Float,
    Int,
    Bool,
    Vec(u8),
    IVec(u8),
    BVec(u8),
    Mat(u8),
    Sampler2D,
    Other(String),
}

impl UType {
    fn parse(s: &str) -> UType {
        match s {
            "float" => UType::Float,
            "int" => UType::Int,
            "bool" => UType::Bool,
            "vec2" => UType::Vec(2),
            "vec3" => UType::Vec(3),
            "vec4" => UType::Vec(4),
            "ivec2" => UType::IVec(2),
            "ivec3" => UType::IVec(3),
            "ivec4" => UType::IVec(4),
            "bvec2" => UType::BVec(2),
            "bvec3" => UType::BVec(3),
            "bvec4" => UType::BVec(4),
            "mat2" => UType::Mat(2),
            "mat3" => UType::Mat(3),
            "mat4" => UType::Mat(4),
            "sampler2D" => UType::Sampler2D,
            o => UType::Other(o.to_string()),
        }
    }
}

/// One `uniform` declaration with its annotations.
#[derive(Debug, Clone, PartialEq)]
pub struct Uniform {
    pub name: String,
    pub ty: UType,
    /// Array length (`uniform vec2 pts[4];`).
    pub array: Option<u32>,
    pub role: Role,
    pub label: Option<String>,
    /// Raw `@default` text.
    pub default: Option<String>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub color: bool,
    pub mipmap: bool,
}

/// Result of [`rewrite`].
#[derive(Debug, Clone)]
pub struct Rewritten {
    /// GLSL 4.50 source for naga (CPU evaluator: host-function stubs).
    pub source: String,
    /// The rewritten user code with `main` renamed `_pen_user_main` (for the GPU source,
    /// see [`crate::gpu`]).
    pub gpu_body: String,
    pub uniforms: Vec<Uniform>,
}

/// Lines added before the user's first line.
pub const PRELUDE_LINES: usize = 2;

/// Globals and host functions every rewritten shader gets (one line).
const PRELUDE: &str = "vec4 _pen_frag_coord; vec4 _pen_frag_color; \
     vec4 _pen_tex(int s, vec2 p) { return vec4(0.0); } \
     vec4 _pen_tex(int s, vec2 p, float bias) { return vec4(0.0); } \
     vec4 _pen_tex_lod(int s, vec2 p, float lod) { return vec4(0.0); } \
     ivec2 _pen_tex_size(int s, int lod) { return ivec2(0); }";

/// GLSL 4.50 keywords that are plain identifiers in GLSL ES 1.0.
const NEW_KEYWORDS: &[&str] = &[
    "sample",
    "patch",
    "buffer",
    "shared",
    "coherent",
    "restrict",
    "readonly",
    "writeonly",
    "subroutine",
    "layout",
    "centroid",
    "flat",
    "smooth",
    "noperspective",
    "resource",
    "common",
    "partition",
    "active",
    "filter",
    "precise",
    "uint",
    "double",
    "image",
    "texture",
];

/// GLSL 4.50 built-in functions missing from ES 1.0: shaders often define their own.
const NEW_BUILTINS: &[&str] = &[
    "round",
    "roundEven",
    "trunc",
    "sinh",
    "cosh",
    "tanh",
    "asinh",
    "acosh",
    "atanh",
    "isnan",
    "isinf",
    "modf",
    "frexp",
    "ldexp",
    "fma",
    "inverse",
    "transpose",
    "determinant",
    "outerProduct",
    "textureLod",
    "texelFetch",
    "textureGrad",
    "textureProj",
    "bitCount",
    "findLSB",
    "findMSB",
    "floatBitsToInt",
    "floatBitsToUint",
    "intBitsToFloat",
    "uintBitsToFloat",
    "packUnorm2x16",
    "unpackUnorm2x16",
    "packSnorm2x16",
    "unpackSnorm2x16",
    "packHalf2x16",
    "unpackHalf2x16",
    "packUnorm4x8",
    "unpackUnorm4x8",
    "saturate",
];

#[derive(Debug, Clone, PartialEq)]
enum T {
    Ident(String),
    /// Comments, whitespace, numbers, punctuation: copied verbatim.
    Text(String),
    /// `/** ... */` comment (copied verbatim; also an annotation block).
    Doc(String),
    /// Preprocessor line (without the newline).
    Directive(String),
}

fn tokenize(src: &str) -> Vec<T> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    let mut line_start = true;
    while i < b.len() {
        let c = b[i];
        if c == b'/' && b.get(i + 1) == Some(&b'*') {
            let end = src[i + 2..].find("*/").map_or(b.len(), |e| i + 2 + e + 2);
            let text = &src[i..end];
            if text.starts_with("/**") && !text.starts_with("/**/") {
                out.push(T::Doc(text.to_string()));
            } else {
                out.push(T::Text(text.to_string()));
            }
            i = end;
            continue;
        }
        if c == b'/' && b.get(i + 1) == Some(&b'/') {
            let end = src[i..].find('\n').map_or(b.len(), |e| i + e);
            out.push(T::Text(src[i..end].to_string()));
            i = end;
            continue;
        }
        if c == b'#' && line_start {
            // Directive up to the end of line (with `\` continuations).
            let mut end = i;
            loop {
                let e = src[end..].find('\n').map_or(b.len(), |e| end + e);
                if e > i && src[..e].trim_end_matches('\r').ends_with('\\') && e < b.len() {
                    end = e + 1;
                    continue;
                }
                end = e;
                break;
            }
            out.push(T::Directive(src[i..end].to_string()));
            i = end;
            continue;
        }
        if c.is_ascii_alphabetic() || c == b'_' {
            let s = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            out.push(T::Ident(src[s..i].to_string()));
            line_start = false;
            continue;
        }
        if c.is_ascii_digit() || (c == b'.' && b.get(i + 1).is_some_and(u8::is_ascii_digit)) {
            // Numbers (with suffixes/exponents) stay one token so `1e5` is not split.
            let s = i;
            while i < b.len()
                && (b[i].is_ascii_alphanumeric()
                    || b[i] == b'.'
                    || ((b[i] == b'+' || b[i] == b'-') && matches!(b[i - 1], b'e' | b'E')))
            {
                i += 1;
            }
            out.push(T::Text(src[s..i].to_string()));
            line_start = false;
            continue;
        }
        if c == b'\n' {
            line_start = true;
        } else if !c.is_ascii_whitespace() {
            line_start = false;
        }
        let ch_len = src[i..].chars().next().map_or(1, char::len_utf8);
        out.push(T::Text(src[i..i + ch_len].to_string()));
        i += ch_len;
    }
    out
}

fn is_space(t: &T) -> bool {
    match t {
        T::Text(s) => s.trim().is_empty() || s.starts_with("//") || s.starts_with("/*"),
        T::Doc(_) => true,
        _ => false,
    }
}

/// Index of the next significant token at or after `i`.
fn next_sig(toks: &[T], mut i: usize) -> Option<usize> {
    while i < toks.len() {
        if !is_space(&toks[i]) {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn parse_doc(doc: &str, u: &mut Uniform) {
    let body = doc
        .trim_start_matches("/**")
        .trim_end_matches("*/")
        .to_string();
    for raw in body.split('\n') {
        let line = raw.trim().trim_start_matches('*').trim();
        // Several tags may share a line (`/** @resolution */`).
        let mut rest = line;
        while let Some(at) = rest.find('@') {
            let after = &rest[at + 1..];
            let tag_end = after
                .find(|c: char| !c.is_ascii_alphanumeric())
                .unwrap_or(after.len());
            let tag = &after[..tag_end];
            let next = after[tag_end..].find(" @").map(|p| tag_end + p + 1);
            let arg = match next {
                Some(n) => after[tag_end..n].trim(),
                None => after[tag_end..].trim(),
            };
            match tag {
                "resolution" => u.role = Role::Resolution,
                "time" => u.role = Role::Time,
                "mouse" => u.role = Role::Mouse,
                "sdf" => u.role = Role::Sdf,
                "mipmap" => u.mipmap = true,
                "color" => u.color = true,
                "label" if !arg.is_empty() => u.label = Some(arg.to_string()),
                "default" if !arg.is_empty() => u.default = Some(arg.to_string()),
                "min" => u.min = arg.parse().ok(),
                "max" => u.max = arg.parse().ok(),
                "range" => {
                    let mut it = arg.split(',').map(|s| s.trim().parse::<f64>().ok());
                    u.min = it.next().flatten();
                    u.max = it.next().flatten();
                }
                _ => {}
            }
            rest = match next {
                Some(n) => &after[n..],
                None => "",
            };
        }
    }
}

/// Rewrites a `.pen` shader into GLSL 4.50 for naga and extracts its uniforms.
pub fn rewrite(src: &str) -> Rewritten {
    let mut toks = tokenize(src);
    let mut uniforms = Vec::new();

    // Function names defined by the shader (`<type> name (` at brace depth 0).
    let mut defined: HashSet<String> = HashSet::new();
    {
        let mut depth = 0i32;
        let sig: Vec<usize> = (0..toks.len()).filter(|&i| !is_space(&toks[i])).collect();
        for w in 0..sig.len() {
            match &toks[sig[w]] {
                T::Text(s) if s == "{" => depth += 1,
                T::Text(s) if s == "}" => depth -= 1,
                T::Ident(name)
                    if depth == 0
                        && w >= 1
                        && w + 1 < sig.len()
                        && matches!(&toks[sig[w - 1]], T::Ident(_))
                        && matches!(&toks[sig[w + 1]], T::Text(s) if s == "(") =>
                {
                    defined.insert(name.clone());
                }
                _ => {}
            }
        }
    }

    let mut i = 0;
    let mut doc: Option<usize> = None;
    let mut depth = 0i32;
    while i < toks.len() {
        match &toks[i] {
            T::Doc(_) => {
                doc = Some(i);
                i += 1;
                continue;
            }
            T::Directive(d) => {
                let d = d.trim_start_matches('#').trim_start();
                if d.starts_with("version") || d.starts_with("extension") {
                    toks[i] = T::Text(String::new());
                }
                i += 1;
                continue;
            }
            T::Text(s) if s == "{" => depth += 1,
            T::Text(s) if s == "}" => depth -= 1,
            T::Ident(id) if id == "precision" => {
                // `precision <q> <type>;` -> removed.
                let mut j = i;
                while j < toks.len() && !matches!(&toks[j], T::Text(s) if s == ";") {
                    toks[j] = T::Text(String::new());
                    j += 1;
                }
                if j < toks.len() {
                    toks[j] = T::Text(String::new());
                }
                i = j + 1;
                continue;
            }
            T::Ident(id) if id == "uniform" && depth == 0 => {
                let doc_text = doc.take().and_then(|d| match &toks[d] {
                    T::Doc(s) => Some(s.clone()),
                    _ => None,
                });
                toks[i] = T::Text(String::new());
                // [precision] type name [ '[' N ']' ] (',' name ...)* ';'
                let mut j = next_sig(&toks, i + 1).unwrap_or(toks.len());
                if let Some(T::Ident(q)) = toks.get(j)
                    && matches!(q.as_str(), "lowp" | "mediump" | "highp")
                {
                    j = next_sig(&toks, j + 1).unwrap_or(toks.len());
                }
                let ty = match toks.get(j) {
                    Some(T::Ident(t)) => UType::parse(t),
                    _ => {
                        i += 1;
                        continue;
                    }
                };
                let mut k = j + 1;
                while let Some(n) = next_sig(&toks, k) {
                    let T::Ident(name) = &toks[n] else { break };
                    let name = name.clone();
                    let mut array = None;
                    let mut m = next_sig(&toks, n + 1).unwrap_or(toks.len());
                    if matches!(toks.get(m), Some(T::Text(s)) if s == "[") {
                        if let Some(len_at) = next_sig(&toks, m + 1)
                            && let T::Text(len) = &toks[len_at]
                        {
                            array = len.parse::<u32>().ok();
                        }
                        while m < toks.len() && !matches!(&toks[m], T::Text(s) if s == "]") {
                            m += 1;
                        }
                        m = next_sig(&toks, m + 1).unwrap_or(toks.len());
                    }
                    let mut u = Uniform {
                        name,
                        ty: ty.clone(),
                        array,
                        role: Role::None,
                        label: None,
                        default: None,
                        min: None,
                        max: None,
                        color: false,
                        mipmap: false,
                    };
                    if let Some(d) = &doc_text {
                        parse_doc(d, &mut u);
                    }
                    uniforms.push(u);
                    if matches!(toks.get(m), Some(T::Text(s)) if s == ",") {
                        k = m + 1;
                        continue;
                    }
                    break;
                }
                i = j;
                continue;
            }
            t if !is_space(t) => doc = None,
            _ => {}
        }
        i += 1;
    }

    let mut out = String::with_capacity(src.len() + 64);
    let mut gpu = String::with_capacity(src.len() + 64);
    for t in &toks {
        let before = out.len();
        match t {
            T::Ident(id) if id == "main" => {
                out.push_str("main");
                gpu.push_str("_pen_user_main");
                continue;
            }
            T::Ident(id) => {
                let r = match id.as_str() {
                    "texture2D" => "_pen_tex",
                    "texture2DLod" | "texture2DLodEXT" => "_pen_tex_lod",
                    "textureSize" => "_pen_tex_size",
                    "sampler2D" => "int",
                    "gl_FragColor" => "_pen_frag_color",
                    "gl_FragCoord" => "_pen_frag_coord",
                    "lowp" | "mediump" | "highp" => "",
                    s if NEW_KEYWORDS.contains(&s)
                        || (NEW_BUILTINS.contains(&s) && defined.contains(s)) =>
                    {
                        out.push_str("_pen_id_");
                        s
                    }
                    s => s,
                };
                out.push_str(r);
            }
            T::Text(s) | T::Doc(s) => out.push_str(s),
            T::Directive(d) => {
                // Identifiers inside `#define` bodies get the same renames.
                if d.trim_start_matches('#').trim_start().starts_with("define") {
                    let inner = rewrite_directive(d, &defined);
                    out.push_str(&inner);
                } else {
                    out.push_str(d);
                }
            }
        }
        gpu.push_str(&out[before..]);
    }
    let mut source = String::with_capacity(out.len() + PRELUDE.len() + 16);
    source.push_str("#version 450\n");
    source.push_str(PRELUDE);
    source.push('\n');
    source.push_str(&out);
    Rewritten {
        source,
        gpu_body: gpu,
        uniforms,
    }
}

fn rewrite_directive(d: &str, defined: &HashSet<String>) -> String {
    let mut out = String::with_capacity(d.len());
    let mut word = String::new();
    let flush = |w: &mut String, out: &mut String| {
        if w.is_empty() {
            return;
        }
        let r = match w.as_str() {
            "texture2D" => "_pen_tex".to_string(),
            "texture2DLod" | "texture2DLodEXT" => "_pen_tex_lod".to_string(),
            "textureSize" => "_pen_tex_size".to_string(),
            "sampler2D" => "int".to_string(),
            "gl_FragColor" => "_pen_frag_color".to_string(),
            "gl_FragCoord" => "_pen_frag_coord".to_string(),
            s if NEW_KEYWORDS.contains(&s)
                || (NEW_BUILTINS.contains(&s) && defined.contains(s)) =>
            {
                format!("_pen_id_{s}")
            }
            s => s.to_string(),
        };
        out.push_str(&r);
        w.clear();
    };
    for c in d.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            word.push(c);
        } else {
            flush(&mut word, &mut out);
            out.push(c);
        }
    }
    flush(&mut word, &mut out);
    out
}

/// A GLSL error with a line in the user's source (1-based, when known).
#[derive(Debug, Clone, PartialEq)]
pub struct GlslError {
    pub message: String,
    pub line: Option<usize>,
}

impl std::fmt::Display for GlslError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.line {
            Some(l) => write!(f, "line {l}: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

/// Parses a rewritten shader with naga.
pub fn parse(rw: &Rewritten) -> Result<naga::Module, GlslError> {
    let mut f = naga::front::glsl::Frontend::default();
    f.parse(
        &naga::front::glsl::Options::from(naga::ShaderStage::Fragment),
        &rw.source,
    )
    .map_err(|e| {
        let first = e.errors.first();
        let line = first.and_then(|er| {
            let loc = er.meta.location(&rw.source);
            (loc.line_number as usize).checked_sub(PRELUDE_LINES)
        });
        let message = e
            .errors
            .iter()
            .take(3)
            .map(|er| er.kind.to_string())
            .collect::<Vec<_>>()
            .join("; ");
        GlslError {
            message: message
                .replace("_pen_tex_size", "textureSize")
                .replace("_pen_tex_lod", "texture2DLod")
                .replace("_pen_tex", "texture2D")
                .replace("_pen_frag_color", "gl_FragColor")
                .replace("_pen_frag_coord", "gl_FragCoord")
                .replace("_pen_id_", ""),
            line: line.filter(|l| *l > 0),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_the_webgl_dialect() {
        let src = "#version 100\nprecision mediump float;\n/** @resolution */\nuniform vec2 u_res;\n\
                   /**\n * @label Image\n * @mipmap\n */\nuniform sampler2D u_img;\n\
                   /**\n * @label Size\n * @default 32\n * @range 1, 64\n */\nuniform highp float u_size;\n\
                   float round(float x) { return floor(x + 0.5); }\n\
                   void main() { vec4 c = texture2D(u_img, gl_FragCoord.xy / u_res); \
                   float sample = round(u_size); gl_FragColor = c * sample; }\n";
        let rw = rewrite(src);
        assert_eq!(rw.uniforms.len(), 3);
        assert_eq!(rw.uniforms[0].role, Role::Resolution);
        assert_eq!(rw.uniforms[1].ty, UType::Sampler2D);
        assert!(rw.uniforms[1].mipmap);
        assert_eq!(rw.uniforms[1].label.as_deref(), Some("Image"));
        assert_eq!(rw.uniforms[2].default.as_deref(), Some("32"));
        assert_eq!(
            (rw.uniforms[2].min, rw.uniforms[2].max),
            (Some(1.0), Some(64.0))
        );
        assert!(rw.source.contains("int u_img;"));
        assert!(rw.source.contains("_pen_tex(u_img"));
        assert!(rw.source.contains("_pen_id_round"));
        assert!(rw.source.contains("_pen_id_sample"));
        assert!(!rw.source.contains("uniform"));
        assert!(!rw.source.contains("precision"));
        // Same number of lines (+ prelude).
        assert_eq!(
            rw.source.lines().count(),
            src.lines().count() + PRELUDE_LINES
        );
        parse(&rw).unwrap();
    }

    #[test]
    fn errors_point_at_user_lines() {
        let rw = rewrite("void main() {\n  gl_FragColor = vec4(1.0);\n  foo();\n}\n");
        let e = parse(&rw).unwrap_err();
        assert_eq!(e.line, Some(3), "{e}");
    }
}
