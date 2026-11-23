//! Error locations inside the snippet and the text of the offending call.

/// File name given to the snippet when it is evaluated.
pub const SNIPPET_FILE: &str = "snippet.js";

/// First `snippet.js:line:col` of a QuickJS stack trace.
pub fn snippet_location(stack: &str) -> Option<(usize, usize)> {
    let needle = format!("{SNIPPET_FILE}:");
    for line in stack.lines() {
        let Some(at) = line.find(&needle) else {
            continue;
        };
        let rest = &line[at + needle.len()..];
        let mut parts = rest.split(|c: char| !c.is_ascii_digit());
        let l = parts.next().and_then(|s| s.parse().ok());
        let c = parts.next().and_then(|s| s.parse().ok());
        if let Some(l) = l {
            return Some((l, c.unwrap_or(1)));
        }
    }
    None
}

/// Source text starting at 1-based `line`:`col`: a call expression
/// `Name(...)` with balanced brackets (strings respected), or the rest of the
/// line. Truncated to ~240 characters.
pub fn call_text(src: &str, line: usize, col: usize) -> Option<String> {
    let mut offset = 0usize;
    for (i, l) in src.split_inclusive('\n').enumerate() {
        if i + 1 == line {
            // QuickJS columns count characters (1-based).
            let char_off = l
                .char_indices()
                .nth(col.saturating_sub(1))
                .map_or(l.len(), |(b, _)| b);
            offset += char_off;
            break;
        }
        offset += l.len();
    }
    let offset = call_start(src, offset.min(src.len()));
    let tail = src.get(offset..)?;
    let text = balanced(tail);
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    Some(truncate(text, 240))
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// QuickJS sometimes reports a column inside the arguments of the failing call
/// (e.g. at the second operand of `Update(a + "/x", ...)`). When `offset` is not
/// at `name(`, backs up to the innermost call on the same line that encloses it.
fn call_start(src: &str, offset: usize) -> usize {
    let Some(tail) = src.get(offset..) else {
        return offset;
    };
    let name_len = tail
        .char_indices()
        .find(|&(_, c)| !(is_ident(c) || c == '.'))
        .map_or(tail.len(), |(i, _)| i);
    if name_len > 0 && tail[name_len..].trim_start().starts_with('(') {
        return offset;
    }
    let line_start = src[..offset].rfind('\n').map_or(0, |i| i + 1);
    let mut open = Vec::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (i, c) in src[line_start..offset].char_indices() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' | '`' => quote = Some(c),
            '(' => open.push(line_start + i),
            ')' => {
                open.pop();
            }
            _ => {}
        }
    }
    for &paren in open.iter().rev() {
        let before = src[..paren].trim_end();
        let start = before
            .char_indices()
            .rev()
            .take_while(|&(_, c)| is_ident(c) || c == '.')
            .last()
            .map(|(i, _)| i);
        if let Some(start) = start
            && before[start..].starts_with(|c: char| c.is_alphabetic() || c == '_' || c == '$')
        {
            return start;
        }
    }
    offset
}

fn balanced(s: &str) -> &str {
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut opened = false;
    for (i, c) in s.char_indices() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' | '`' => quote = Some(c),
            '(' | '[' | '{' => {
                depth += 1;
                opened = true;
            }
            ')' | ']' | '}' => {
                depth -= 1;
                if depth < 0 {
                    return &s[..i];
                }
                if depth == 0 && opened && c == ')' {
                    return &s[..i + 1];
                }
            }
            '\n' | ';' if depth == 0 => return &s[..i],
            _ => {}
        }
        if i > 4000 {
            return &s[..i];
        }
    }
    s
}

pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max).collect();
    format!("{cut}...")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locate_and_extract() {
        let stack = "    at call (prelude.js:10:13)\n    at Update (prelude.js:50:7)\n    at <eval> (snippet.js:2:3)\n";
        assert_eq!(snippet_location(stack), Some((2, 3)));
        let src = "const a = 1;\n  Update(\"x\", {fill: \"#f)f\", n: [1, 2]}); Print(a)\n";
        assert_eq!(
            call_text(src, 2, 3).as_deref(),
            Some("Update(\"x\", {fill: \"#f)f\", n: [1, 2]})")
        );
        assert_eq!(snippet_location("    at snippet.js:1:9\n"), Some((1, 9)));
        // A column inside the arguments backs up to the enclosing call.
        let src = "x = 1\nUpdate(i + \"/Description\", {content: \"(a)\"})\n";
        assert_eq!(
            call_text(src, 2, 12).as_deref(),
            Some("Update(i + \"/Description\", {content: \"(a)\"})")
        );
        let src = "const n = Get(page, n => n.id)[0]; Insert(n.id + \"/q\", {})";
        assert_eq!(
            call_text(src, 1, 50).as_deref(),
            Some("Insert(n.id + \"/q\", {})")
        );
    }
}
