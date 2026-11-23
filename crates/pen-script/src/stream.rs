//! Statement-boundary splitter for streamed snippets.
//!
//! While an agent is still "typing" a snippet, [`complete_prefix`] tells how much
//! of the received text consists of complete top-level statements, so those can
//! be evaluated live (in the same context) and the rest later. It is a small
//! tokenizer, not a parser: it understands strings, template literals (with
//! `${}` nesting), comments, regex literals, bracket depth, `if`/`for`/`while`
//! headers and statements that may continue on the next token (`else`, `catch`,
//! `finally`, `.method()`, binary operators). A boundary created by a newline or
//! a closing `}` is only confirmed once the next token has arrived.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Prev {
    /// Start of a statement.
    Start,
    /// A token that can end an expression (identifier, literal, `)`, `]`, `}`).
    Value,
    /// An operator, keyword or opening bracket: more must follow.
    Op,
}

const CONTINUE_KEYWORDS: &[&str] = &["else", "catch", "finally", "instanceof", "in", "of"];
const HEADER_KEYWORDS: &[&str] = &["if", "for", "while", "with", "switch", "catch"];
const OP_KEYWORDS: &[&str] = &[
    "const",
    "let",
    "var",
    "if",
    "else",
    "for",
    "while",
    "do",
    "new",
    "typeof",
    "void",
    "delete",
    "in",
    "of",
    "instanceof",
    "function",
    "class",
    "extends",
    "case",
    "throw",
    "try",
    "await",
    "yield",
    "switch",
    "with",
    "export",
    "import",
];

fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c == b'$' || c >= 0x80
}

fn is_ident(c: u8) -> bool {
    is_ident_start(c) || c.is_ascii_digit()
}

/// Byte length of the longest prefix of `src` made of complete top-level
/// statements (0 if none is complete yet). Always on a UTF-8 boundary.
pub fn complete_prefix(src: &str) -> usize {
    let b = src.as_bytes();
    let n = b.len();
    let mut i = 0;
    // Open brackets: b'(' b'[' b'{', or b'$' for a template `${` expression.
    let mut stack: Vec<u8> = Vec::new();
    let mut last = 0usize;
    let mut pending: Option<usize> = None;
    let mut prev = Prev::Start;
    let mut stmt_kw: Option<&str> = None;
    // A header keyword was seen at the top level; its `(...)` is the header.
    let mut header_kw = false;
    let mut header_depth_open = false;
    // The statement needs a body (after `if (...)`, `else`, `do`...).
    let mut needs_body = false;

    macro_rules! reset {
        () => {{
            prev = Prev::Start;
            stmt_kw = None;
            header_kw = false;
            header_depth_open = false;
            needs_body = false;
        }};
    }

    while i < n {
        let c = b[i];
        // Whitespace.
        if c == b'\n' {
            if stack.is_empty() && prev == Prev::Value && !needs_body && pending.is_none() {
                pending = Some(i + 1);
            }
            i += 1;
            continue;
        }
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        // Comments.
        if c == b'/' && i + 1 < n && b[i + 1] == b'/' {
            while i < n && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if c == b'/' && i + 1 < n && b[i + 1] == b'*' {
            match src[i + 2..].find("*/") {
                Some(e) => {
                    i = i + 2 + e + 2;
                    continue;
                }
                None => return last,
            }
        }
        if c == b'/' && i + 1 >= n {
            // Could become a comment or an operator.
            return last;
        }

        // A new token starts: settle a pending boundary.
        if let Some(p) = pending {
            let word_end = {
                let mut e = i;
                while e < n && is_ident(b[e]) {
                    e += 1;
                }
                e
            };
            let word = &src[i..word_end];
            let continues = if is_ident_start(c) {
                if word_end == n
                    && (CONTINUE_KEYWORDS.iter().any(|k| k.starts_with(word))
                        || "while".starts_with(word))
                {
                    // The word may still grow (`el` -> `else`).
                    return last;
                }
                CONTINUE_KEYWORDS.contains(&word) || (word == "while" && stmt_kw == Some("do"))
            } else {
                matches!(
                    c,
                    b'.' | b'('
                        | b'['
                        | b'?'
                        | b':'
                        | b','
                        | b'='
                        | b'+'
                        | b'-'
                        | b'*'
                        | b'/'
                        | b'%'
                        | b'&'
                        | b'|'
                        | b'^'
                        | b'<'
                        | b'>'
                        | b'`'
                ) || (c == b'!' && i + 1 < n && b[i + 1] == b'=')
                    || (c == b'!' && i + 1 >= n)
            };
            pending = None;
            if !continues {
                last = p;
                reset!();
            }
        }

        match c {
            b'"' | b'\'' => {
                let mut j = i + 1;
                let mut closed = false;
                while j < n {
                    match b[j] {
                        b'\\' => j += 2,
                        x if x == c => {
                            closed = true;
                            break;
                        }
                        b'\n' => break,
                        _ => j += 1,
                    }
                }
                if !closed {
                    return last;
                }
                i = j + 1;
                needs_body = false;
                prev = Prev::Value;
            }
            b'`' => {
                needs_body = false;
                match scan_template(b, i + 1) {
                    Template::End(j) => {
                        i = j;
                        prev = Prev::Value;
                    }
                    Template::Expr(j) => {
                        stack.push(b'$');
                        i = j;
                        prev = Prev::Op;
                    }
                    Template::Open => return last,
                }
            }
            b'(' | b'[' | b'{' => {
                if stack.is_empty() {
                    if c == b'(' && header_kw && !header_depth_open {
                        header_depth_open = true;
                    } else {
                        needs_body = false;
                    }
                }
                stack.push(c);
                prev = if c == b'{' && stack.len() == 1 {
                    Prev::Start
                } else {
                    Prev::Op
                };
                i += 1;
            }
            b')' | b']' | b'}' => {
                let Some(open) = stack.pop() else {
                    // Unbalanced: stop at what we have.
                    return last;
                };
                i += 1;
                if open == b'$' {
                    // End of a `${...}` inside a template: keep scanning the template.
                    match scan_template(b, i) {
                        Template::End(j) => {
                            i = j;
                            prev = Prev::Value;
                        }
                        Template::Expr(j) => {
                            stack.push(b'$');
                            i = j;
                            prev = Prev::Op;
                        }
                        Template::Open => return last,
                    }
                    continue;
                }
                if stack.is_empty() {
                    if c == b')' && header_depth_open && header_kw {
                        // `if (...)`: the body follows.
                        header_kw = false;
                        needs_body = true;
                        prev = Prev::Op;
                        continue;
                    }
                    prev = Prev::Value;
                    if c == b'}' {
                        pending = Some(i);
                    }
                } else {
                    prev = Prev::Value;
                }
            }
            b';' => {
                i += 1;
                if stack.is_empty() {
                    last = i;
                    pending = None;
                    reset!();
                } else {
                    prev = Prev::Op;
                }
            }
            b'/' => {
                if prev == Prev::Value {
                    i += 1;
                    if i < n && b[i] == b'=' {
                        i += 1;
                    }
                    prev = Prev::Op;
                } else {
                    // Regex literal.
                    let mut j = i + 1;
                    let mut class = false;
                    let mut closed = false;
                    while j < n {
                        match b[j] {
                            b'\\' => j += 2,
                            b'[' => {
                                class = true;
                                j += 1;
                            }
                            b']' => {
                                class = false;
                                j += 1;
                            }
                            b'/' if !class => {
                                closed = true;
                                break;
                            }
                            b'\n' => break,
                            _ => j += 1,
                        }
                    }
                    if !closed {
                        return last;
                    }
                    j += 1;
                    while j < n && is_ident(b[j]) {
                        j += 1;
                    }
                    i = j;
                    needs_body = false;
                    prev = Prev::Value;
                }
            }
            c if is_ident_start(c) => {
                let s = i;
                while i < n && is_ident(b[i]) {
                    i += 1;
                }
                let word = &src[s..i];
                if stack.is_empty() {
                    if prev == Prev::Start && stmt_kw.is_none() {
                        stmt_kw = Some(match word {
                            "do" => "do",
                            "if" => "if",
                            "try" => "try",
                            _ => "",
                        });
                    }
                    if HEADER_KEYWORDS.contains(&word) && prev != Prev::Value {
                        header_kw = true;
                        header_depth_open = false;
                    } else {
                        needs_body = matches!(word, "else" | "do" | "try" | "finally");
                    }
                }
                prev = if OP_KEYWORDS.contains(&word) {
                    Prev::Op
                } else {
                    Prev::Value
                };
            }
            c if c.is_ascii_digit() => {
                while i < n && (b[i].is_ascii_alphanumeric() || b[i] == b'.' || b[i] == b'_') {
                    i += 1;
                }
                needs_body = false;
                prev = Prev::Value;
            }
            _ => {
                // Punctuator / operator.
                let two = if i + 1 < n {
                    &b[i..i + 2]
                } else {
                    &b[i..i + 1]
                };
                if two == b"++" || two == b"--" {
                    i += 2;
                    prev = if prev == Prev::Value {
                        Prev::Value
                    } else {
                        Prev::Op
                    };
                } else {
                    i += 1;
                    prev = Prev::Op;
                }
            }
        }
    }
    // A pending boundary stays unconfirmed until the next token arrives.
    last
}

enum Template {
    /// Closing backtick found; index after it.
    End(usize),
    /// `${` found; index after it.
    Expr(usize),
    /// Not closed yet.
    Open,
}

fn scan_template(b: &[u8], mut j: usize) -> Template {
    while j < b.len() {
        match b[j] {
            b'\\' => j += 2,
            b'`' => return Template::End(j + 1),
            b'$' if j + 1 < b.len() && b[j + 1] == b'{' => return Template::Expr(j + 2),
            _ => j += 1,
        }
    }
    Template::Open
}

#[cfg(test)]
mod tests {
    use super::complete_prefix as cp;

    fn split(src: &str) -> &str {
        &src[..cp(src)]
    }

    #[test]
    fn semicolons_and_newlines() {
        assert_eq!(split("a = 1; b = 2"), "a = 1;");
        assert_eq!(split("a = 1\nb = 2"), "a = 1\n");
        assert_eq!(split("a = 1\nb = 2\n"), "a = 1\n");
        assert_eq!(split("a = 1\nb = 2\nx"), "a = 1\nb = 2\n");
        assert_eq!(split("a = 1\nb = 2\nc"), "a = 1\n");
        assert_eq!(split("Insert(p, {\n type: \"frame\"\n"), "");
        assert_eq!(
            split("x = Insert(p, {\n type: \"frame\"\n})\nPrint(x)"),
            "x = Insert(p, {\n type: \"frame\"\n})\n"
        );
    }

    #[test]
    fn continuations() {
        assert_eq!(split("a\n  .map(f)\nb"), "a\n  .map(f)\n");
        assert_eq!(
            split("if (a) {\n x()\n}\nelse {\n y()\n}\nz"),
            "if (a) {\n x()\n}\nelse {\n y()\n}"
        );
        assert_eq!(split("if (a) {\n x()\n}\n"), "");
        assert_eq!(split("if (a) {\n x()\n}\nel"), "");
        assert_eq!(split("if (a)\n x()\nz()\n"), "if (a)\n x()\n");
        assert_eq!(
            split("try { a() } catch (e) { b() }\nc()"),
            "try { a() } catch (e) { b() }"
        );
        assert_eq!(
            split("for (const x of xs) {\n f(x)\n}\ng()"),
            "for (const x of xs) {\n f(x)\n}"
        );
        assert_eq!(split("const t = a\n+ b\nc()"), "const t = a\n+ b\n");
    }

    #[test]
    fn strings_templates_comments_regex() {
        assert_eq!(split("a = \"x;y\"; b"), "a = \"x;y\";");
        assert_eq!(split("a = \"x;"), "");
        assert_eq!(
            split("a = `x ${ {b:1}.b } ;`; c"),
            "a = `x ${ {b:1}.b } ;`;"
        );
        assert_eq!(split("a = `x ${ y"), "");
        assert_eq!(split("// hi; there\na = 1; b"), "// hi; there\na = 1;");
        assert_eq!(split("/* ; */ a = 1; b"), "/* ; */ a = 1;");
        assert_eq!(split("r = /;[/]/g; b"), "r = /;[/]/g;");
        assert_eq!(split("x = a / b; c"), "x = a / b;");
    }
}
