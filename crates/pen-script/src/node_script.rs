//! Runner of `script` nodes (procedural generators): [`ScriptNodeRunner`] implements
//! [`pen_layout::ScriptRunner`] on QuickJS.
//!
//! Contract (same as the legacy editor, see `docs/scripts-shaders.md`):
//! - the file starts with a `/** ... */` header with `@schema <version>` (required) and
//!   typed `@input name: type[(args)] [= default]` lines;
//! - the body runs as `((pencil) => { <body> })({ width, height, input })` and must
//!   return an array of `.pen` nodes (schema `@schema`, migrated to the current one);
//! - `Math.random` is mulberry32 reset to seed 0 before every run (deterministic);
//! - limits: 16 MiB heap, 512 KiB stack, 2 s, 1000 output nodes; no `reusable` nodes,
//!   no connections; ids are dropped.
//!
//! Compiled scripts (one QuickJS runtime + context each) are cached by source text, so
//! re-running a script with other inputs or another size only calls the function.

use std::collections::{HashMap, VecDeque};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pen_doc::ParamValue;
use pen_layout::{ScriptError, ScriptRequest, ScriptRunner};
use pen_schema::{CURRENT_VERSION, MigrateCx, Node};
use rquickjs::context::EvalOptions;
use rquickjs::{CatchResultExt, CaughtError, Context, Function, Runtime};
use serde_json::{Map, Value, json};

/// Limits of one script node run.
#[derive(Debug, Clone, Copy)]
pub struct NodeScriptLimits {
    pub memory_bytes: usize,
    pub stack_bytes: usize,
    pub timeout: Duration,
    pub max_nodes: usize,
}

impl Default for NodeScriptLimits {
    fn default() -> Self {
        Self {
            memory_bytes: 16 * 1024 * 1024,
            stack_bytes: 512 * 1024,
            timeout: Duration::from_secs(2),
            max_nodes: 1000,
        }
    }
}

/// Type of a declared `@input`.
#[derive(Debug, Clone, PartialEq)]
pub enum InputType {
    Number { min: Option<f64>, max: Option<f64> },
    String,
    Boolean,
    Color,
    Ref,
    Enum(Vec<String>),
}

/// One `@input` of a script header.
#[derive(Debug, Clone, PartialEq)]
pub struct InputDef {
    pub name: String,
    pub ty: InputType,
    pub default: Value,
}

/// Parsed `/** ... */` header of a script file.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ScriptHeader {
    pub schema: Option<String>,
    pub inputs: Vec<InputDef>,
}

fn header_error(msg: impl Into<String>) -> ScriptError {
    ScriptError::new(msg, None)
}

// ------------------------------------------------------------------ header lexer

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    Number(String),
    Str(String),
    Color(String),
    LParen,
    RParen,
    Comma,
    Colon,
    Equals,
    True,
    False,
    End,
}

fn describe_tok(t: &Tok) -> String {
    match t {
        Tok::Ident(s) | Tok::Number(s) | Tok::Color(s) if !s.is_empty() => format!("'{s}'"),
        Tok::Str(s) => format!("'{s}'"),
        Tok::LParen => "'('".into(),
        Tok::RParen => "')'".into(),
        Tok::Comma => "','".into(),
        Tok::Colon => "':'".into(),
        Tok::Equals => "'='".into(),
        Tok::True => "'true'".into(),
        Tok::False => "'false'".into(),
        Tok::End => "end of line".into(),
        _ => "token".into(),
    }
}

struct Lexer<'a> {
    src: &'a [u8],
    pos: usize,
    peeked: Option<Tok>,
}

fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c == b'$'
}

fn is_ident(c: u8) -> bool {
    is_ident_start(c) || c.is_ascii_digit()
}

impl<'a> Lexer<'a> {
    fn new(s: &'a str) -> Self {
        Self {
            src: s.as_bytes(),
            pos: 0,
            peeked: None,
        }
    }

    fn peek(&mut self) -> Result<&Tok, ScriptError> {
        if self.peeked.is_none() {
            let t = self.lex()?;
            self.peeked = Some(t);
        }
        Ok(self.peeked.as_ref().expect("peeked"))
    }

    fn next(&mut self) -> Result<Tok, ScriptError> {
        match self.peeked.take() {
            Some(t) => Ok(t),
            None => self.lex(),
        }
    }

    fn text(&self, a: usize) -> String {
        String::from_utf8_lossy(&self.src[a..self.pos]).into_owned()
    }

    fn keyword(&self, kw: &str) -> bool {
        let end = self.pos + kw.len();
        self.src.get(self.pos..end) == Some(kw.as_bytes())
            && self.src.get(end).is_none_or(|c| !is_ident(*c))
    }

    fn lex(&mut self) -> Result<Tok, ScriptError> {
        while self.pos < self.src.len() && self.src[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
        let Some(&c) = self.src.get(self.pos) else {
            return Ok(Tok::End);
        };
        let start = self.pos;
        let single = match c {
            b'(' => Some(Tok::LParen),
            b')' => Some(Tok::RParen),
            b',' => Some(Tok::Comma),
            b':' => Some(Tok::Colon),
            b'=' => Some(Tok::Equals),
            _ => None,
        };
        if let Some(t) = single {
            self.pos += 1;
            return Ok(t);
        }
        if c == b'"' {
            self.pos += 1;
            let mut out = Vec::new();
            while self.pos < self.src.len() && self.src[self.pos] != b'"' {
                if self.src[self.pos] == b'\\' && self.pos + 1 < self.src.len() {
                    out.push(self.src[self.pos + 1]);
                    self.pos += 2;
                } else {
                    out.push(self.src[self.pos]);
                    self.pos += 1;
                }
            }
            if self.pos >= self.src.len() {
                return Err(header_error("Unterminated string"));
            }
            self.pos += 1;
            return Ok(Tok::Str(String::from_utf8_lossy(&out).into_owned()));
        }
        if c == b'#' {
            self.pos += 1;
            while self.pos < self.src.len() && self.src[self.pos].is_ascii_hexdigit() {
                self.pos += 1;
            }
            if self.pos == start + 1 {
                return Err(header_error("Expected hex digits after '#'"));
            }
            return Ok(Tok::Color(self.text(start)));
        }
        if c == b'-' || c == b'.' || c.is_ascii_digit() {
            if c == b'-' || c == b'.' {
                self.pos += 1;
            }
            while self.pos < self.src.len() && self.src[self.pos].is_ascii_digit() {
                self.pos += 1;
            }
            if self.src.get(self.pos) == Some(&b'.') {
                self.pos += 1;
                while self.pos < self.src.len() && self.src[self.pos].is_ascii_digit() {
                    self.pos += 1;
                }
            }
            let s = self.text(start);
            if s == "-" || s == "." {
                return Err(header_error(format!("Invalid number '{s}'")));
            }
            return Ok(Tok::Number(s));
        }
        if self.keyword("true") {
            self.pos += 4;
            return Ok(Tok::True);
        }
        if self.keyword("false") {
            self.pos += 5;
            return Ok(Tok::False);
        }
        if is_ident_start(c) {
            self.pos += 1;
            while self.pos < self.src.len() && is_ident(self.src[self.pos]) {
                self.pos += 1;
            }
            return Ok(Tok::Ident(self.text(start)));
        }
        let ch = std::str::from_utf8(&self.src[start..])
            .ok()
            .and_then(|s| s.chars().next())
            .unwrap_or('?');
        Err(header_error(format!("Unexpected character '{ch}'")))
    }
}

fn is_literal(t: &Tok) -> bool {
    matches!(
        t,
        Tok::Number(_) | Tok::Str(_) | Tok::Color(_) | Tok::Ident(_) | Tok::True | Tok::False
    )
}

/// JavaScript `parseFloat` of a header number token / input string (longest numeric
/// prefix, NaN when there is none).
fn parse_float(s: &str) -> f64 {
    let s = s.trim_start();
    let b = s.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    if s[i..].starts_with("Infinity") {
        return if b.first() == Some(&b'-') {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        };
    }
    let digits_start = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
    }
    if i == digits_start || (i == digits_start + 1 && b[digits_start] == b'.') {
        return f64::NAN;
    }
    // Exponent.
    let mut j = i;
    if j < b.len() && (b[j] == b'e' || b[j] == b'E') {
        j += 1;
        if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
            j += 1;
        }
        let e0 = j;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
        if j > e0 {
            i = j;
        }
    }
    s[..i].parse().unwrap_or(f64::NAN)
}

/// Parses one `@input` directive (the text after `@input`).
fn parse_input(line: &str) -> Result<InputDef, ScriptError> {
    let mut lx = Lexer::new(line);
    let name = match lx.next()? {
        Tok::Ident(n) => n,
        t => {
            return Err(header_error(format!(
                "@input must start with a name, got {}",
                describe_tok(&t)
            )));
        }
    };
    let t = lx.next()?;
    if t != Tok::Colon {
        return Err(header_error(format!(
            "@input '{name}': expected ':', got {}",
            describe_tok(&t)
        )));
    }
    let ty = match lx.next()? {
        Tok::Ident(t) => t,
        _ => return Err(header_error(format!("@input '{name}' is missing a type"))),
    };
    let mut positional: Vec<Tok> = Vec::new();
    let mut named: Vec<(String, Tok)> = Vec::new();
    if *lx.peek()? == Tok::LParen {
        lx.next()?;
        if *lx.peek()? == Tok::RParen {
            lx.next()?;
        } else {
            loop {
                let t = lx.next()?;
                if let Tok::Ident(arg) = &t
                    && *lx.peek()? == Tok::Equals
                {
                    lx.next()?;
                    let v = lx.next()?;
                    if !is_literal(&v) {
                        return Err(header_error(format!(
                            "@input '{name}' arg '{arg}' must be a literal, got {}",
                            describe_tok(&v)
                        )));
                    }
                    named.push((arg.clone(), v));
                } else {
                    if !is_literal(&t) {
                        return Err(header_error(format!(
                            "@input '{name}' arg must be a literal, got {}",
                            describe_tok(&t)
                        )));
                    }
                    positional.push(t);
                }
                if *lx.peek()? == Tok::Comma {
                    lx.next()?;
                    continue;
                }
                break;
            }
            let t = lx.next()?;
            if t != Tok::RParen {
                return Err(header_error(format!(
                    "@input '{name}' arg list: expected ')', got {}",
                    describe_tok(&t)
                )));
            }
        }
    }
    let default = if *lx.peek()? == Tok::Equals {
        lx.next()?;
        Some(lx.next()?)
    } else {
        None
    };
    let t = lx.peek()?.clone();
    if t != Tok::End {
        return Err(header_error(format!(
            "@input '{name}' has unexpected trailing token {}",
            describe_tok(&t)
        )));
    }
    let no_args = |positional: &[Tok], named: &[(String, Tok)]| {
        if positional.is_empty() && named.is_empty() {
            Ok(())
        } else {
            Err(header_error(format!(
                "@input '{name}' of type '{ty}' does not accept arguments"
            )))
        }
    };
    let (ty, default) = match ty.as_str() {
        "number" => {
            if !positional.is_empty() {
                return Err(header_error(format!(
                    "@input number '{name}' does not accept positional arguments"
                )));
            }
            let (mut min, mut max) = (None, None);
            for (arg, v) in &named {
                let Tok::Number(n) = v else {
                    return Err(header_error(format!(
                        "@input number '{name}' arg '{arg}' must be a number, got {}",
                        describe_tok(v)
                    )));
                };
                match arg.as_str() {
                    "min" => min = Some(parse_float(n)),
                    "max" => max = Some(parse_float(n)),
                    _ => {
                        return Err(header_error(format!(
                            "@input number '{name}' has unknown arg '{arg}'. Valid args: min, max"
                        )));
                    }
                }
            }
            let d = match &default {
                None => 0.0,
                Some(Tok::Number(n)) => parse_float(n),
                Some(t) => {
                    return Err(header_error(format!(
                        "@input '{name}' default must be a number, got {}",
                        describe_tok(t)
                    )));
                }
            };
            (InputType::Number { min, max }, json!(d))
        }
        "string" => {
            no_args(&positional, &named)?;
            let d = match &default {
                None => String::new(),
                Some(Tok::Str(s)) => s.clone(),
                Some(t) => {
                    return Err(header_error(format!(
                        "@input '{name}' string default must be a quoted string, got {}",
                        describe_tok(t)
                    )));
                }
            };
            (InputType::String, json!(d))
        }
        "boolean" => {
            no_args(&positional, &named)?;
            let d = match &default {
                None | Some(Tok::False) => false,
                Some(Tok::True) => true,
                Some(t) => {
                    return Err(header_error(format!(
                        "@input '{name}' default must be 'true' or 'false', got {}",
                        describe_tok(t)
                    )));
                }
            };
            (InputType::Boolean, json!(d))
        }
        "color" => {
            no_args(&positional, &named)?;
            let d = match &default {
                None => "#000000".to_string(),
                Some(Tok::Color(s) | Tok::Str(s)) => s.clone(),
                Some(t) => {
                    return Err(header_error(format!(
                        "@input '{name}' color default must be a hex literal like #RRGGBB, got {}",
                        describe_tok(t)
                    )));
                }
            };
            (InputType::Color, json!(d))
        }
        "ref" => {
            no_args(&positional, &named)?;
            let d = match &default {
                None => String::new(),
                Some(Tok::Str(s) | Tok::Ident(s)) => s.clone(),
                Some(t) => {
                    return Err(header_error(format!(
                        "@input '{name}' ref default must be an identifier or quoted string, got {}",
                        describe_tok(t)
                    )));
                }
            };
            (InputType::Ref, json!(d))
        }
        "enum" => {
            if !named.is_empty() {
                return Err(header_error(format!(
                    "@input enum '{name}' does not accept named arguments"
                )));
            }
            if positional.is_empty() {
                return Err(header_error(format!(
                    "@input enum '{name}' must specify at least one option"
                )));
            }
            let mut opts = Vec::new();
            for t in &positional {
                let Tok::Str(s) = t else {
                    return Err(header_error(format!(
                        "@input enum '{name}' option must be a quoted string, got {}",
                        describe_tok(t)
                    )));
                };
                opts.push(s.clone());
            }
            let d = match &default {
                None => opts[0].clone(),
                Some(Tok::Str(s)) => {
                    if !opts.contains(s) {
                        return Err(header_error(format!(
                            "@input enum '{name}' default '{s}' is not one of the options"
                        )));
                    }
                    s.clone()
                }
                Some(t) => {
                    return Err(header_error(format!(
                        "@input '{name}' enum default must be a quoted string, got {}",
                        describe_tok(t)
                    )));
                }
            };
            (InputType::Enum(opts), json!(d))
        }
        other => {
            return Err(header_error(format!(
                "Invalid @input type '{other}'. Must be one of: number, string, boolean, color, ref, enum"
            )));
        }
    };
    Ok(InputDef { name, ty, default })
}

/// Parses every `/** ... */` block of a script: `@schema` and `@input` lines (later
/// declarations of the same input win).
pub fn parse_header(src: &str) -> Result<ScriptHeader, ScriptError> {
    let mut h = ScriptHeader::default();
    let mut rest = src;
    while let Some(a) = rest.find("/**") {
        let after = &rest[a + 3..];
        let Some(b) = after.find("*/") else { break };
        let block = &after[..b];
        rest = &after[b + 2..];
        for raw in block.split('\n') {
            let line = raw.trim_start();
            let line = line.strip_prefix('*').unwrap_or(line);
            let line = line.strip_prefix(' ').unwrap_or(line).trim();
            let Some(tag) = line.strip_prefix('@') else {
                continue;
            };
            let end = tag
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(tag.len());
            if end == 0 {
                continue;
            }
            let (name, arg) = (&tag[..end], tag[end..].trim());
            match name {
                "schema" => h.schema = Some(arg.to_string()),
                "input" => {
                    let d = parse_input(arg)?;
                    match h.inputs.iter_mut().find(|i| i.name == d.name) {
                        Some(e) => *e = d,
                        None => h.inputs.push(d),
                    }
                }
                _ => {}
            }
        }
    }
    Ok(h)
}

/// Input values passed to the script: declared inputs only, defaults for missing ones,
/// numbers coerced and clamped, enums checked.
pub fn coerce_inputs(
    header: &ScriptHeader,
    given: &[(Arc<str>, ParamValue)],
) -> Result<Map<String, Value>, ScriptError> {
    let mut out = Map::new();
    for d in &header.inputs {
        let v = given
            .iter()
            .rev()
            .find(|(k, _)| **k == *d.name)
            .map(|(_, v)| param_json(v))
            .unwrap_or_else(|| d.default.clone());
        let v = match &d.ty {
            InputType::Number { min, max } => {
                let mut n = match &v {
                    Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
                    Value::String(s) => parse_float(s),
                    Value::Array(a) if a.len() == 1 => match &a[0] {
                        Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
                        _ => f64::NAN,
                    },
                    _ => f64::NAN,
                };
                if n.is_nan() {
                    n = min.unwrap_or(0.0);
                }
                if let Some(m) = min
                    && n < *m
                {
                    n = *m;
                }
                if let Some(m) = max
                    && n > *m
                {
                    n = *m;
                }
                serde_json::Number::from_f64(n).map_or(Value::Null, Value::Number)
            }
            InputType::Enum(opts) => {
                let ok = v.as_str().is_some_and(|s| opts.iter().any(|o| o == s));
                if !ok {
                    let shown = match &v {
                        Value::String(s) => s.clone(),
                        v => v.to_string(),
                    };
                    return Err(ScriptError::new(
                        "Invalid input value",
                        Some(format!(
                            "Value '{shown}' is not allowed for input '{}'. Must be one of: {}",
                            d.name,
                            opts.join(", ")
                        )),
                    ));
                }
                v
            }
            _ => v,
        };
        out.insert(d.name.clone(), v);
    }
    Ok(out)
}

fn param_json(v: &ParamValue) -> Value {
    match v {
        ParamValue::Num(n) => serde_json::Number::from_f64(*n).map_or(Value::Null, Value::Number),
        ParamValue::Bool(b) => Value::Bool(*b),
        ParamValue::Str(s) => Value::String(s.to_string()),
        ParamValue::Vec(v) => Value::Array(
            v.iter()
                .map(|x| serde_json::Number::from_f64(*x).map_or(Value::Null, Value::Number))
                .collect(),
        ),
    }
}

// ------------------------------------------------------------------ runtime

const RNG_PRELUDE: &str = r#"(function () {
  let state = 0;
  Math.random = function () {
    state = (state + 0x6D2B79F5) | 0;
    let t = Math.imul(state ^ (state >>> 15), 1 | state);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
  globalThis.__resetRng = function (seed) { state = seed | 0; };
})();"#;

/// A compiled script: its own runtime/context and the wrapper function.
struct Compiled {
    /// The wrapper function is the global `__run` of `ctx`.
    ctx: Context,
    rt: Runtime,
    deadline: Arc<Mutex<Option<Instant>>>,
    header: ScriptHeader,
}

/// Max compiled scripts kept alive.
const COMPILED_CAP: usize = 16;

/// [`ScriptRunner`] for `script` nodes on QuickJS. `Send`; one instance per pipeline.
pub struct ScriptNodeRunner {
    pub limits: NodeScriptLimits,
    compiled: HashMap<u64, Result<Arc<Compiled>, ScriptError>>,
    order: VecDeque<u64>,
}

impl Default for ScriptNodeRunner {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(dead_code)]
fn assert_send() {
    fn is_send<T: Send>() {}
    is_send::<ScriptNodeRunner>();
}

fn caught_message(e: CaughtError<'_>) -> String {
    match e {
        CaughtError::Exception(ex) => {
            let name: Option<String> = ex.get::<_, Option<String>>("name").ok().flatten();
            let msg = ex.message().unwrap_or_default();
            let mut s = match name {
                Some(n) => format!("{n}: {msg}"),
                None => msg,
            };
            if let Some(st) = ex.stack()
                && !st.trim().is_empty()
            {
                s.push('\n');
                s.push_str(st.trim_end());
            }
            s
        }
        CaughtError::Value(v) => {
            if v.is_null() {
                "out of memory".to_string()
            } else if let Some(s) = v.as_string() {
                s.to_string().unwrap_or_default()
            } else {
                format!("{v:?}")
            }
        }
        CaughtError::Error(e) => e.to_string(),
    }
}

/// Error details: the message, the offending lines of the source, then the stack.
fn with_excerpt(source: &str, details: String) -> String {
    let Some(code) = code_excerpt(source, &details) else {
        return details;
    };
    match details.split_once('\n') {
        Some((first, rest)) => format!("{first}\n{code}\n{rest}"),
        None => format!("{details}\n{code}"),
    }
}

/// `> 12: code` excerpt around the first `script.js:<line>` of a stack trace.
fn code_excerpt(source: &str, details: &str) -> Option<String> {
    let at = details.find("script.js:")?;
    let rest = &details[at + "script.js:".len()..];
    let line: usize = rest
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()?;
    let lines: Vec<&str> = source.split('\n').collect();
    // The wrapper adds two lines before the body.
    let line = line.checked_sub(2)?;
    if line == 0 || line > lines.len() {
        return None;
    }
    let from = line.saturating_sub(2);
    let to = (line + 1).min(lines.len());
    let mut out = String::new();
    for (i, l) in lines.iter().enumerate().take(to).skip(from) {
        if !out.is_empty() {
            out.push('\n');
        }
        let mark = if i + 1 == line { '>' } else { ' ' };
        out.push_str(&format!("{mark} {}: {}", i + 1, l.trim_end()));
    }
    Some(out)
}

impl ScriptNodeRunner {
    pub fn new() -> Self {
        Self {
            limits: NodeScriptLimits::default(),
            compiled: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn compiled(&mut self, source: &str) -> Result<Arc<Compiled>, ScriptError> {
        let mut hs = DefaultHasher::new();
        source.hash(&mut hs);
        let key = hs.finish();
        if let Some(c) = self.compiled.get(&key) {
            return c.clone();
        }
        let c = self.compile(source).map(Arc::new);
        self.compiled.insert(key, c.clone());
        self.order.push_back(key);
        while self.order.len() > COMPILED_CAP {
            if let Some(old) = self.order.pop_front() {
                self.compiled.remove(&old);
            }
        }
        c
    }

    fn compile(&self, source: &str) -> Result<Compiled, ScriptError> {
        let header = parse_header(source)?;
        if header.schema.is_none() {
            return Err(ScriptError::new(
                "Missing @schema version tag",
                Some(format!(
                    "Add /** @schema {CURRENT_VERSION} */ to the top of the script."
                )),
            ));
        }
        let internal = |e: rquickjs::Error| ScriptError::new(format!("QuickJS: {e}"), None);
        let rt = Runtime::new().map_err(internal)?;
        rt.set_memory_limit(self.limits.memory_bytes);
        rt.set_max_stack_size(self.limits.stack_bytes);
        let deadline: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
        let d = deadline.clone();
        rt.set_interrupt_handler(Some(Box::new(move || {
            d.lock()
                .ok()
                .and_then(|g| *g)
                .is_some_and(|t| Instant::now() > t)
        })));
        let ctx = Context::full(&rt).map_err(internal)?;
        // The body starts on line 3 of `script.js` (see `code_excerpt`).
        let wrapper = [
            "(function (__input) {
",
            "globalThis.__resetRng(0); const __result = ((pencil) => {
",
            source,
            "
})(JSON.parse(__input));
",
            "if (__result === undefined) { throw new Error(\"Script must return an array of nodes. ",
            "Add a 'return [...]' statement at the end of your script.\"); }
",
            "return JSON.stringify(__result);
})",
        ]
        .concat();
        ctx.with(|ctx| -> Result<(), ScriptError> {
            let mut o = EvalOptions::default();
            o.filename = Some("prelude.js".into());
            ctx.eval_with_options::<rquickjs::Value, _>(RNG_PRELUDE, o)
                .catch(&ctx)
                .map_err(|e| ScriptError::new("Script compile error", Some(caught_message(e))))?;
            let mut o = EvalOptions::default();
            o.filename = Some("script.js".into());
            o.strict = false;
            let f: Function = ctx
                .eval_with_options(wrapper.as_str(), o)
                .catch(&ctx)
                .map_err(|e| {
                    ScriptError::new(
                        "Script compile error",
                        Some(with_excerpt(source, caught_message(e))),
                    )
                })?;
            ctx.globals().set("__run", f).map_err(internal)?;
            Ok(())
        })?;
        Ok(Compiled {
            ctx,
            rt,
            deadline,
            header,
        })
    }

    /// Runs a script source directly (without the pipeline's caches).
    pub fn run_source(
        &mut self,
        source: &str,
        inputs: &[(Arc<str>, ParamValue)],
        width: f32,
        height: f32,
    ) -> Result<Vec<Node>, ScriptError> {
        let c = self.compiled(source)?;
        let input = coerce_inputs(&c.header, inputs)?;
        let arg = json!({ "width": width, "height": height, "input": input }).to_string();
        if let Ok(mut g) = c.deadline.lock() {
            *g = Some(Instant::now() + self.limits.timeout);
        }
        let result = c.ctx.with(|ctx| -> Result<String, ScriptError> {
            let f: Function = ctx
                .globals()
                .get("__run")
                .map_err(|e| ScriptError::new(format!("QuickJS: {e}"), None))?;
            f.call::<_, String>((arg,)).catch(&ctx).map_err(|e| {
                let d = caught_message(e);
                let d = if d.contains("interrupted") {
                    format!(
                        "Script took longer than {} s and was stopped.\n{d}",
                        self.limits.timeout.as_secs_f32()
                    )
                } else {
                    d
                };
                ScriptError::new("Script runtime error", Some(with_excerpt(source, d)))
            })
        });
        if let Ok(mut g) = c.deadline.lock() {
            *g = None;
        }
        // Keep the heap small between runs.
        c.rt.run_gc();
        let text = result?;
        let schema = c.header.schema.clone().unwrap_or_default();
        prepare_output(&text, &schema, self.limits.max_nodes)
    }
}

impl ScriptRunner for ScriptNodeRunner {
    fn run(&mut self, req: &ScriptRequest<'_>) -> Result<Vec<Node>, ScriptError> {
        self.run_source(req.source, req.inputs, req.width, req.height)
    }
}

/// Validates and migrates a script's JSON output into schema nodes.
pub fn prepare_output(
    json_text: &str,
    schema: &str,
    max_nodes: usize,
) -> Result<Vec<Node>, ScriptError> {
    let invalid = |d: &str| ScriptError::new("Invalid script output", Some(d.to_string()));
    let v: Value = serde_json::from_str(json_text).map_err(|e| invalid(&e.to_string()))?;
    let Value::Array(mut nodes) = v else {
        return Err(invalid("Script must return an array of nodes."));
    };
    if schema != CURRENT_VERSION {
        let mut doc = json!({ "version": schema, "children": nodes });
        let mut cx = MigrateCx::new();
        pen_schema::migrate(&mut doc, &mut cx).map_err(|e| {
            ScriptError::new(
                "Unsupported @schema version",
                Some(format!("{e} (in the @schema tag of the script)")),
            )
        })?;
        nodes = match doc.get_mut("children").map(Value::take) {
            Some(Value::Array(a)) => a,
            _ => Vec::new(),
        };
    }
    let mut count = 0usize;
    for n in &mut nodes {
        check_node(n, &mut count, max_nodes)?;
    }
    Ok(nodes
        .into_iter()
        .map(|v| pen_schema::node_from_value(v, "script output").0)
        .collect())
}

fn check_node(v: &mut Value, count: &mut usize, max: usize) -> Result<(), ScriptError> {
    let Value::Object(m) = v else {
        return Err(ScriptError::new(
            "Invalid script output",
            Some("Every element of the returned array must be a node object.".into()),
        ));
    };
    *count += 1;
    if *count > max {
        return Err(ScriptError::new(
            "Too many nodes",
            Some(format!("Script output cannot exceed {max} nodes.")),
        ));
    }
    if m.get("reusable").is_some_and(|r| r.as_bool() == Some(true)) {
        return Err(ScriptError::new(
            "Invalid script output",
            Some("Reusable nodes are not supported in script outputs.".into()),
        ));
    }
    if m.get("type").and_then(Value::as_str) == Some("connection") {
        return Err(ScriptError::new(
            "Invalid script output",
            Some("Connection nodes are not supported in script outputs.".into()),
        ));
    }
    m.remove("id");
    if let Some(Value::Array(c)) = m.get_mut("children") {
        for c in c {
            check_node(c, count, max)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_inputs() {
        let h = parse_header(
            "/**\n * @schema 2.11\n * @input rows: number(min=1, max=10) = 3\n * @input color: color = #3B82F6\n \
             * @input label: string = \"Hi \\\"x\\\"\"\n * @input on: boolean = true\n \
             * @input mode: enum(\"a\", \"b\") = \"b\"\n * @input target: ref\n */\nreturn [];",
        )
        .unwrap();
        assert_eq!(h.schema.as_deref(), Some("2.11"));
        assert_eq!(h.inputs.len(), 6);
        assert_eq!(
            h.inputs[0].ty,
            InputType::Number {
                min: Some(1.0),
                max: Some(10.0)
            }
        );
        assert_eq!(h.inputs[0].default, json!(3.0));
        assert_eq!(h.inputs[1].default, json!("#3B82F6"));
        assert_eq!(h.inputs[2].default, json!("Hi \"x\""));
        assert_eq!(h.inputs[3].default, json!(true));
        assert_eq!(h.inputs[4].default, json!("b"));
        assert_eq!(h.inputs[5].ty, InputType::Ref);
    }

    #[test]
    fn header_errors() {
        for (src, msg) in [
            ("/** @input x: vector */", "Invalid @input type 'vector'"),
            ("/** @input x: number(foo=1) */", "unknown arg 'foo'"),
            ("/** @input x: enum() */", "at least one option"),
            (
                "/** @input x: enum(\"a\") = \"z\" */",
                "is not one of the options",
            ),
            ("/** @input x: string = 3 */", "must be a quoted string"),
            ("/** @input x: number = 1 2 */", "unexpected trailing token"),
            ("/** @input 3: number */", "must start with a name"),
        ] {
            let e = parse_header(src).unwrap_err();
            assert!(e.title.contains(msg), "{src}: {}", e.title);
        }
    }

    #[test]
    fn number_coercion_clamps_and_parses() {
        let h = parse_header("/** @schema 2.11\n * @input n: number(min=2, max=5) = 3 */").unwrap();
        let run = |v: ParamValue| coerce_inputs(&h, &[("n".into(), v)]).unwrap()["n"].clone();
        assert_eq!(run(ParamValue::Num(9.0)), json!(5.0));
        assert_eq!(run(ParamValue::Num(-1.0)), json!(2.0));
        assert_eq!(run(ParamValue::Str("4.5px".into())), json!(4.5));
        assert_eq!(run(ParamValue::Str("abc".into())), json!(2.0));
        assert_eq!(run(ParamValue::Bool(true)), json!(2.0));
        assert_eq!(coerce_inputs(&h, &[]).unwrap()["n"], json!(3.0));
    }

    #[test]
    fn parse_float_matches_js() {
        assert_eq!(parse_float("12px"), 12.0);
        assert_eq!(parse_float(" -3.5e2x"), -350.0);
        assert_eq!(parse_float(".5"), 0.5);
        assert!(parse_float("x1").is_nan());
        assert!(parse_float(".").is_nan());
        assert_eq!(parse_float("1e"), 1.0);
    }
}
