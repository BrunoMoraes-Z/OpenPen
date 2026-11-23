//! JSON helpers: JSONC stripping, JS-compatible output formatting, semantic
//! comparison and a few JavaScript built-ins the migrations rely on.

use std::io;

use serde::Serialize;
use serde_json::Value;
use serde_json::ser::{Formatter, PrettyFormatter};

/// Object map type used throughout the crate (insertion-ordered).
pub type JsonMap = serde_json::Map<String, Value>;

/// Replaces `//` and `/* */` comments outside of strings with spaces, keeping
/// newlines so that error positions stay meaningful. Mirrors the legacy loader,
/// which retries `JSON.parse` on the comment-stripped text.
pub fn strip_jsonc_comments(src: &str) -> String {
    let bytes = src.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    let mut in_str = false;
    while i < bytes.len() {
        let b = bytes[i];
        if in_str {
            out.push(b);
            if b == b'\\' && i + 1 < bytes.len() {
                out.push(bytes[i + 1]);
                i += 2;
                continue;
            }
            if b == b'"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        match b {
            b'"' => {
                in_str = true;
                out.push(b);
                i += 1;
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    out.push(if bytes[i] == b'\r' { b'\r' } else { b' ' });
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                out.extend_from_slice(b"  ");
                i += 2;
                while i < bytes.len() && !(bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/')) {
                    out.push(if matches!(bytes[i], b'\n' | b'\r') {
                        bytes[i]
                    } else {
                        b' '
                    });
                    i += 1;
                }
                if i < bytes.len() {
                    out.extend_from_slice(b"  ");
                    i += 2;
                }
            }
            _ => {
                out.push(b);
                i += 1;
            }
        }
    }
    // Only ASCII bytes were replaced by ASCII bytes, and multi-byte sequences are
    // either copied verbatim or replaced byte-by-byte inside comments (where every
    // byte becomes a space), so the output is valid UTF-8.
    String::from_utf8(out).unwrap_or_default()
}

/// Writes `v` the way JavaScript's `Number.prototype.toString` (and therefore
/// `JSON.stringify`) does: integers without a fractional part, shortest
/// round-trip digits, exponent notation only outside `1e-7 < |v| < 1e21`.
pub fn write_js_number<W: ?Sized + io::Write>(w: &mut W, v: f64) -> io::Result<()> {
    if !v.is_finite() {
        return w.write_all(b"null");
    }
    if v == 0.0 {
        return w.write_all(b"0");
    }
    if v.fract() == 0.0 && v.abs() < 9.007_199_254_740_992e15 {
        return write!(w, "{}", v as i64);
    }
    // Shortest round-trip digits in scientific form, e.g. "-1.25e-7".
    let sci = format!("{v:e}");
    let (neg, sci) = match sci.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, sci.as_str()),
    };
    let (mant, exp) = sci.split_once('e').unwrap_or((sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i32;
    let n = exp + 1;
    let mut s = String::with_capacity(32);
    if neg {
        s.push('-');
    }
    if k <= n && n <= 21 {
        s.push_str(&digits);
        for _ in 0..(n - k) {
            s.push('0');
        }
    } else if 0 < n && n <= 21 {
        s.push_str(&digits[..n as usize]);
        s.push('.');
        s.push_str(&digits[n as usize..]);
    } else if -6 < n && n <= 0 {
        s.push_str("0.");
        for _ in 0..(-n) {
            s.push('0');
        }
        s.push_str(&digits);
    } else {
        s.push_str(&digits[..1]);
        if k > 1 {
            s.push('.');
            s.push_str(&digits[1..]);
        }
        s.push('e');
        s.push(if n > 0 { '+' } else { '-' });
        s.push_str(&(n - 1).abs().to_string());
    }
    w.write_all(s.as_bytes())
}

/// Formats a number like JavaScript's `String(n)`.
pub fn js_number_string(v: f64) -> String {
    let mut buf = Vec::with_capacity(24);
    let _ = write_js_number(&mut buf, v);
    String::from_utf8(buf).unwrap_or_default()
}

/// `serde_json` formatter that reproduces `JSON.stringify(value, null, 2)`.
pub struct JsFormatter {
    inner: PrettyFormatter<'static>,
}

impl JsFormatter {
    pub fn new() -> Self {
        JsFormatter {
            inner: PrettyFormatter::with_indent(b"  "),
        }
    }
}

impl Default for JsFormatter {
    fn default() -> Self {
        Self::new()
    }
}

impl Formatter for JsFormatter {
    fn write_f64<W: ?Sized + io::Write>(&mut self, w: &mut W, v: f64) -> io::Result<()> {
        write_js_number(w, v)
    }
    fn write_f32<W: ?Sized + io::Write>(&mut self, w: &mut W, v: f32) -> io::Result<()> {
        write_js_number(w, v as f64)
    }
    fn begin_array<W: ?Sized + io::Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.inner.begin_array(w)
    }
    fn end_array<W: ?Sized + io::Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.inner.end_array(w)
    }
    fn begin_array_value<W: ?Sized + io::Write>(
        &mut self,
        w: &mut W,
        first: bool,
    ) -> io::Result<()> {
        self.inner.begin_array_value(w, first)
    }
    fn end_array_value<W: ?Sized + io::Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.inner.end_array_value(w)
    }
    fn begin_object<W: ?Sized + io::Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.inner.begin_object(w)
    }
    fn end_object<W: ?Sized + io::Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.inner.end_object(w)
    }
    fn begin_object_key<W: ?Sized + io::Write>(
        &mut self,
        w: &mut W,
        first: bool,
    ) -> io::Result<()> {
        self.inner.begin_object_key(w, first)
    }
    fn end_object_key<W: ?Sized + io::Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.inner.end_object_key(w)
    }
    fn begin_object_value<W: ?Sized + io::Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.inner.begin_object_value(w)
    }
    fn end_object_value<W: ?Sized + io::Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.inner.end_object_value(w)
    }
}

/// Serializes any value as pretty JSON with JS number formatting (2-space indent).
pub fn to_js_pretty<T: Serialize + ?Sized>(value: &T) -> Vec<u8> {
    let mut out = Vec::with_capacity(4096);
    let mut ser = serde_json::Serializer::with_formatter(&mut out, JsFormatter::new());
    // Serializing to a Vec<u8> only fails on non-string map keys, which this
    // crate never produces.
    let _ = value.serialize(&mut ser);
    out
}

/// Semantic JSON equality: object key order is ignored and numbers compare by
/// value (`1` == `1.0`).
pub fn json_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => match (x.as_f64(), y.as_f64()) {
            (Some(x), Some(y)) => x == y,
            _ => x == y,
        },
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(a, b)| json_eq(a, b))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get(k).is_some_and(|w| json_eq(v, w)))
        }
        _ => a == b,
    }
}

/// Returns the first path where `a` and `b` differ (for test diagnostics).
pub fn json_diff(a: &Value, b: &Value) -> Option<String> {
    fn go(a: &Value, b: &Value, path: &mut String) -> Option<String> {
        match (a, b) {
            (Value::Array(x), Value::Array(y)) => {
                if x.len() != y.len() {
                    return Some(format!("{path}: array length {} != {}", x.len(), y.len()));
                }
                for (i, (a, b)) in x.iter().zip(y).enumerate() {
                    let len = path.len();
                    path.push_str(&format!("[{i}]"));
                    if let Some(d) = go(a, b, path) {
                        return Some(d);
                    }
                    path.truncate(len);
                }
                None
            }
            (Value::Object(x), Value::Object(y)) => {
                for (k, v) in x {
                    let len = path.len();
                    path.push('.');
                    path.push_str(k);
                    match y.get(k) {
                        None => return Some(format!("{path}: missing on the right")),
                        Some(w) => {
                            if let Some(d) = go(v, w, path) {
                                return Some(d);
                            }
                        }
                    }
                    path.truncate(len);
                }
                y.keys()
                    .find(|k| !x.contains_key(*k))
                    .map(|k| format!("{path}.{k}: missing on the left"))
            }
            _ if json_eq(a, b) => None,
            _ => Some(format!("{path}: {a} != {b}")),
        }
    }
    go(a, b, &mut String::new())
}

/// JavaScript truthiness of a JSON value (`undefined` is represented by `None`).
pub fn js_truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(_)) | Some(Value::Object(_)) => true,
    }
}

/// JavaScript `encodeURIComponent`.
pub fn encode_uri_component(s: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        let keep = b.is_ascii_alphanumeric()
            || matches!(
                b,
                b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')'
            );
        if keep {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 15) as usize] as char);
        }
    }
    out
}

/// Builds a JSON number from an `f64`, using an integer representation when the
/// value is integral (so that `Value` comparisons and output stay JS-like).
pub fn number_value(v: f64) -> Value {
    if v.fract() == 0.0 && v.abs() < 9.007_199_254_740_992e15 {
        Value::from(v as i64)
    } else {
        serde_json::Number::from_f64(v)
            .map(Value::Number)
            .unwrap_or(Value::Null)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn js_numbers() {
        let cases: &[(f64, &str)] = &[
            (0.0, "0"),
            (-0.0, "0"),
            (1.0, "1"),
            (-12.0, "-12"),
            (1.5, "1.5"),
            (1.399999976158142, "1.399999976158142"),
            (0.1, "0.1"),
            (0.000001, "0.000001"),
            (0.0000001, "1e-7"),
            (1.5e-10, "1.5e-10"),
            (1e21, "1e+21"),
            (1e20, "100000000000000000000"),
            (123456.789, "123456.789"),
            (-0.5, "-0.5"),
        ];
        for (v, s) in cases {
            assert_eq!(js_number_string(*v), *s, "{v}");
        }
    }

    #[test]
    fn strips_comments() {
        let src = "{\n // c\n \"a\": \"//not\", /* b\n */ \"b\": 1 }";
        let v: Value = serde_json::from_str(&strip_jsonc_comments(src)).unwrap();
        assert_eq!(v, json!({"a": "//not", "b": 1}));
    }

    #[test]
    fn pretty_matches_js() {
        let v = json!({"a": [1, 2.5, {"b": []}], "c": {}, "d": "x\u{1}"});
        let s = String::from_utf8(to_js_pretty(&v)).unwrap();
        assert_eq!(
            s,
            "{\n  \"a\": [\n    1,\n    2.5,\n    {\n      \"b\": []\n    }\n  ],\n  \"c\": {},\n  \"d\": \"x\\u0001\"\n}"
        );
    }

    #[test]
    fn semantic_eq() {
        assert!(json_eq(
            &json!({"a": 1, "b": [1.0]}),
            &json!({"b": [1], "a": 1.0})
        ));
        assert!(!json_eq(&json!({"a": 1}), &json!({"a": 2})));
    }

    #[test]
    fn uri_component() {
        assert_eq!(encode_uri_component("my lib (1).pen"), "my%20lib%20(1).pen");
        assert_eq!(encode_uri_component("ç"), "%C3%A7");
    }
}
