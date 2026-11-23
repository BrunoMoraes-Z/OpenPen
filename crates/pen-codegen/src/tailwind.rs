//! CSS declarations -> Tailwind v4 utility classes.
//!
//! Common properties map to their utilities (`flex`, `items-center`, `gap-[12px]`,
//! `bg-(--brand)`...); anything else becomes an arbitrary property (`[prop:value]`).

use crate::model::Decl;

/// Arbitrary-value escaping: spaces -> `_`, literal `_` -> `\_`, `"` -> `'`.
pub(crate) fn esc(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for ch in v.chars() {
        match ch {
            '_' => out.push_str("\\_"),
            ' ' => out.push('_'),
            '"' => out.push('\''),
            c => out.push(c),
        }
    }
    out
}

/// `--x` when `v` is exactly `var(--x)`.
fn bare_var(v: &str) -> Option<&str> {
    let inner = v.strip_prefix("var(")?.strip_suffix(')')?;
    (inner.starts_with("--") && !inner.contains([' ', ',', '('])).then_some(inner)
}

/// `prefix-0`, `prefix-(--x)` or `prefix-[value]`.
fn value_util(prefix: &str, v: &str) -> String {
    if v == "0" {
        return format!("{prefix}-0");
    }
    match bare_var(v) {
        Some(var) => format!("{prefix}-({var})"),
        None => format!("{prefix}-[{}]", esc(v)),
    }
}

/// Like [`value_util`] with a type hint for ambiguous utilities (`text-[length:...]`).
fn typed_util(prefix: &str, hint: &str, v: &str) -> String {
    match bare_var(v) {
        Some(var) => format!("{prefix}-({hint}:{var})"),
        None if v.starts_with("calc(") || v.starts_with("var(") => {
            format!("{prefix}-[{hint}:{}]", esc(v))
        }
        None => format!("{prefix}-[{}]", esc(v)),
    }
}

fn arbitrary(prop: &str, v: &str) -> String {
    format!("[{prop}:{}]", esc(v))
}

/// Splits a CSS value on top-level spaces (not inside parentheses).
pub(crate) fn split_top(v: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    for (i, ch) in v.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth -= 1,
            ' ' if depth == 0 => {
                if i > start {
                    out.push(&v[start..i]);
                }
                start = i + 1;
            }
            _ => {}
        }
    }
    if start < v.len() {
        out.push(&v[start..]);
    }
    out
}

fn sides(prefixes: [&str; 7], v: &str, out: &mut Vec<String>) {
    // prefixes: all, y, x, top, right, bottom, left
    let t = split_top(v);
    match t.as_slice() {
        [a] => out.push(value_util(prefixes[0], a)),
        [y, x] => {
            out.push(value_util(prefixes[1], y));
            out.push(value_util(prefixes[2], x));
        }
        [t, r, b, l] => {
            for (p, v) in [
                (prefixes[3], t),
                (prefixes[4], r),
                (prefixes[5], b),
                (prefixes[6], l),
            ] {
                out.push(value_util(p, v));
            }
        }
        _ => out.push(arbitrary("padding", v)),
    }
}

fn size_util(prefix: &str, v: &str) -> String {
    match v {
        "100%" => format!("{prefix}-full"),
        "fit-content" => format!("{prefix}-fit"),
        _ => value_util(prefix, v),
    }
}

fn one(decl: &Decl, out: &mut Vec<String>) {
    let v = decl.value.as_str();
    let mapped: Option<String> = match (decl.prop, v) {
        ("position", "absolute" | "relative") => Some(v.into()),
        ("left", _) => Some(value_util("left", v)),
        ("top", _) => Some(value_util("top", v)),
        ("display", "flex") => Some("flex".into()),
        ("display", "block") => Some("block".into()),
        ("flex-direction", "column") => Some("flex-col".into()),
        ("flex-direction", "row") => Some("flex-row".into()),
        ("justify-content", "center") => Some("justify-center".into()),
        ("justify-content", "flex-end") => Some("justify-end".into()),
        ("justify-content", "flex-start") => Some("justify-start".into()),
        ("justify-content", "space-between") => Some("justify-between".into()),
        ("justify-content", "space-around") => Some("justify-around".into()),
        ("align-items", "flex-start") => Some("items-start".into()),
        ("align-items", "center") => Some("items-center".into()),
        ("align-items", "flex-end") => Some("items-end".into()),
        ("align-self", "stretch") => Some("self-stretch".into()),
        ("gap", _) => Some(value_util("gap", v)),
        ("padding", _) => {
            sides(["p", "py", "px", "pt", "pr", "pb", "pl"], v, out);
            return;
        }
        ("width", _) => Some(size_util("w", v)),
        ("height", _) => Some(size_util("h", v)),
        ("min-width", "0") => Some("min-w-0".into()),
        ("min-height", "0") => Some("min-h-0".into()),
        ("flex", "1 1 0") => Some("flex-1".into()),
        ("flex-shrink", "0") => Some("shrink-0".into()),
        ("background-color", _) => Some(typed_util("bg", "color", v)),
        ("color", "transparent") => Some("text-transparent".into()),
        ("color", _) => Some(typed_util("text", "color", v)),
        ("border-radius", "50%") => Some("rounded-full".into()),
        ("border-radius", _) => Some(value_util("rounded", v)),
        ("overflow", "hidden") => Some("overflow-hidden".into()),
        ("overflow", "visible") => Some("overflow-visible".into()),
        ("opacity", _) => Some(value_util("opacity", v)),
        ("box-shadow", _) => Some(value_util("shadow", v)),
        ("backdrop-filter", _) if v.starts_with("blur(") && split_top(v).len() == 1 => {
            Some(value_util("backdrop-blur", &v["blur(".len()..v.len() - 1]))
        }
        ("-webkit-backdrop-filter", _) => return,
        ("filter", _) if v.starts_with("blur(") && split_top(v).len() == 1 => {
            Some(value_util("blur", &v["blur(".len()..v.len() - 1]))
        }
        ("mix-blend-mode", _) => Some(format!("mix-blend-{v}")),
        ("font-size", _) => Some(typed_util("text", "length", v)),
        ("font-weight", "100") => Some("font-thin".into()),
        ("font-weight", "200") => Some("font-extralight".into()),
        ("font-weight", "300") => Some("font-light".into()),
        ("font-weight", "400") => Some("font-normal".into()),
        ("font-weight", "500") => Some("font-medium".into()),
        ("font-weight", "600") => Some("font-semibold".into()),
        ("font-weight", "700") => Some("font-bold".into()),
        ("font-weight", "800") => Some("font-extrabold".into()),
        ("font-weight", "900") => Some("font-black".into()),
        ("font-style", "italic") => Some("italic".into()),
        ("line-height", _) => Some(value_util("leading", v)),
        ("letter-spacing", _) => Some(value_util("tracking", v)),
        ("text-align", "center" | "right" | "justify" | "left") => Some(format!("text-{v}")),
        ("white-space", "pre") => Some("whitespace-pre".into()),
        ("white-space", "pre-wrap") => Some("whitespace-pre-wrap".into()),
        ("text-decoration-line", _) => {
            out.extend(v.split(' ').map(str::to_string));
            return;
        }
        ("object-fit", "cover" | "contain" | "fill") => Some(format!("object-{v}")),
        ("transform-origin", "0 0") => Some("origin-top-left".into()),
        ("-webkit-background-clip", "text") => return,
        ("background-clip", "text") => Some("bg-clip-text".into()),
        ("background-image", _) if v.contains("gradient(") || v.starts_with("url(") => {
            Some(format!("bg-[{}]", esc(v)))
        }
        ("background-size", "cover" | "contain") => Some(format!("bg-{v}")),
        ("background-position", "center") => Some("bg-center".into()),
        ("border", "0") => Some("border-0".into()),
        _ => None,
    };
    out.push(mapped.unwrap_or_else(|| arbitrary(decl.prop, v)));
}

/// Utility classes for a list of declarations (in declaration order).
pub(crate) fn classes(decls: &[Decl]) -> String {
    let mut out = Vec::new();
    for d in decls {
        one(d, &mut out);
    }
    out.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(prop: &'static str, value: &str) -> Decl {
        Decl {
            prop,
            value: value.into(),
        }
    }

    #[test]
    fn utilities() {
        let c = classes(&[
            d("position", "relative"),
            d("display", "flex"),
            d("flex-direction", "column"),
            d("align-items", "center"),
            d("gap", "12px"),
            d("padding", "8px 16px"),
            d("width", "320px"),
            d("height", "fit-content"),
            d("background-color", "var(--brand)"),
            d("border-radius", "8px 8px 0 0"),
            d("box-shadow", "0 4px 12px 0 #00000040"),
            d("font-family", "\"Space Grotesk\", sans-serif"),
            d("font-size", "calc(var(--fs) * 1px)"),
            d("font-weight", "600"),
            d("color", "#111111"),
            d("flex", "1 1 0"),
            d("min-width", "0"),
            d("outline", "1px solid #e5e7eb"),
            d("outline-offset", "-1px"),
        ]);
        assert_eq!(
            c,
            "relative flex flex-col items-center gap-[12px] py-[8px] px-[16px] w-[320px] h-fit \
             bg-(color:--brand) rounded-[8px_8px_0_0] shadow-[0_4px_12px_0_#00000040] \
             [font-family:'Space_Grotesk',_sans-serif] text-[length:calc(var(--fs)_*_1px)] \
             font-semibold text-[#111111] flex-1 min-w-0 [outline:1px_solid_#e5e7eb] \
             [outline-offset:-1px]"
        );
    }

    #[test]
    fn split_and_escape() {
        assert_eq!(
            split_top("calc(var(--a) * 1px) 4px"),
            vec!["calc(var(--a) * 1px)", "4px"]
        );
        assert_eq!(esc("a_b c"), "a\\_b_c");
        assert_eq!(
            classes(&[d("padding", "4px 0 2px 1px")]),
            "pt-[4px] pr-0 pb-[2px] pl-[1px]"
        );
        assert_eq!(classes(&[d("filter", "blur(4px)")]), "blur-[4px]");
        assert_eq!(classes(&[d("left", "-10px")]), "left-[-10px]");
    }
}
