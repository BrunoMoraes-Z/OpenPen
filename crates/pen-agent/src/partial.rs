//! Reading the script of an `execute` call while the model is still writing it.
//!
//! The model streams the tool input as JSON text (`input_json_delta`). To run
//! the script as it arrives (`ScriptEngine::execute_partial`), the app needs
//! the decoded value of the top-level `input` string from a JSON object that
//! is usually cut off somewhere in the middle.

/// Returns the longest decoded prefix of the top-level `input` string field
/// of `partial_json`, a JSON object that may be truncated anywhere.
///
/// - `None` while the `input` key has not been reached (or its value has not
///   started), when `input` is not a string, or when the text is not a JSON
///   object.
/// - The value is decoded (`\n`, `\"`, `\uXXXX`, surrogate pairs). An escape
///   sequence cut off at the end (`\`, `\u00`, a high surrogate without its
///   low half yet) is left out until it is complete.
/// - Other fields may come before or after `input`, in any order; nested
///   objects and arrays are skipped (a nested `input` key is ignored).
pub fn execute_input_prefix(partial_json: &str) -> Option<String> {
    let mut s = Scanner {
        b: partial_json.as_bytes(),
        src: partial_json,
        i: 0,
    };
    s.ws();
    if s.bump()? != b'{' {
        return None;
    }
    loop {
        s.ws();
        match s.peek()? {
            b'}' => return None,
            b',' => {
                s.i += 1;
                continue;
            }
            b'"' => {}
            _ => return None,
        }
        // Key.
        s.i += 1;
        let (key, complete) = s.string();
        if !complete {
            return None;
        }
        s.ws();
        if s.bump()? != b':' {
            return None;
        }
        s.ws();
        if key == "input" {
            if s.peek()? != b'"' {
                return None;
            }
            s.i += 1;
            return Some(s.string().0);
        }
        if !s.skip_value()? {
            return None;
        }
    }
}

struct Scanner<'a> {
    b: &'a [u8],
    src: &'a str,
    i: usize,
}

impl Scanner<'_> {
    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let c = self.peek()?;
        self.i += 1;
        Some(c)
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    /// Decodes a string whose opening quote was consumed. Returns the decoded
    /// text so far and whether the closing quote was found.
    fn string(&mut self) -> (String, bool) {
        let mut out = String::new();
        loop {
            // Copy a run of plain characters at once (valid UTF-8 boundaries:
            // `"` and `\` are ASCII and never inside a multi-byte sequence).
            let start = self.i;
            while let Some(c) = self.peek() {
                if c == b'"' || c == b'\\' {
                    break;
                }
                self.i += 1;
            }
            out.push_str(&self.src[start..self.i]);
            match self.peek() {
                None => return (out, false),
                Some(b'"') => {
                    self.i += 1;
                    return (out, true);
                }
                Some(_) => {
                    // Backslash.
                    let Some(ch) = self.escape() else {
                        return (out, false);
                    };
                    if let Some(ch) = ch {
                        out.push(ch);
                    }
                }
            }
        }
    }

    /// Decodes one escape sequence starting at a backslash. `None` when it is
    /// cut off (the position is left at the backslash); `Some(None)` for an
    /// invalid sequence, which is skipped.
    fn escape(&mut self) -> Option<Option<char>> {
        let at = self.i;
        let code = *self.b.get(at + 1)?;
        let simple = match code {
            b'"' => Some('"'),
            b'\\' => Some('\\'),
            b'/' => Some('/'),
            b'b' => Some('\u{8}'),
            b'f' => Some('\u{c}'),
            b'n' => Some('\n'),
            b'r' => Some('\r'),
            b't' => Some('\t'),
            b'u' => None,
            _ => {
                // Invalid escape: keep the character, as lenient parsers do.
                self.i = at + 2;
                return Some((code as char).is_ascii().then_some(code as char));
            }
        };
        if let Some(c) = simple {
            self.i = at + 2;
            return Some(Some(c));
        }
        let hi = self.hex4(at + 2)?;
        if (0xD800..0xDC00).contains(&hi) {
            // High surrogate: wait for `\uDC00..\uDFFF`.
            if self.b.len() < at + 12 {
                // Not enough bytes yet; but bail out early if what is there
                // cannot start a low surrogate escape.
                let rest = &self.b[at + 6..];
                let pattern = b"\\u";
                let n = rest.len().min(2);
                if rest[..n] != pattern[..n] {
                    self.i = at + 6;
                    return Some(Some('\u{FFFD}'));
                }
                return None;
            }
            if &self.b[at + 6..at + 8] == b"\\u" {
                let lo = self.hex4(at + 8)?;
                if (0xDC00..0xE000).contains(&lo) {
                    self.i = at + 12;
                    let c = 0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00);
                    return Some(char::from_u32(c));
                }
            }
            self.i = at + 6;
            return Some(Some('\u{FFFD}'));
        }
        self.i = at + 6;
        if (0xDC00..0xE000).contains(&hi) {
            return Some(Some('\u{FFFD}'));
        }
        Some(char::from_u32(hi))
    }

    fn hex4(&self, from: usize) -> Option<u32> {
        let digits = self.b.get(from..from + 4)?;
        let mut v = 0u32;
        for &d in digits {
            v = v * 16 + (d as char).to_digit(16).unwrap_or(0);
        }
        Some(v)
    }

    /// Skips one JSON value. `Some(true)` when it is complete, `None` when the
    /// text ends inside it, `Some(false)` on a syntax error.
    fn skip_value(&mut self) -> Option<bool> {
        match self.peek()? {
            b'"' => {
                self.i += 1;
                let (_, done) = self.string();
                done.then_some(true)
            }
            b'{' | b'[' => {
                let mut depth = 0usize;
                loop {
                    match self.bump()? {
                        b'{' | b'[' => depth += 1,
                        b'}' | b']' => {
                            depth -= 1;
                            if depth == 0 {
                                return Some(true);
                            }
                        }
                        b'"' => {
                            let (_, done) = self.string();
                            if !done {
                                return None;
                            }
                        }
                        _ => {}
                    }
                }
            }
            b'-' | b'0'..=b'9' | b't' | b'f' | b'n' => {
                while let Some(c) = self.peek() {
                    if matches!(c, b',' | b'}' | b']' | b' ' | b'\t' | b'\n' | b'\r') {
                        return Some(true);
                    }
                    self.i += 1;
                }
                // A number or literal may still be growing.
                None
            }
            _ => Some(false),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn full(v: &serde_json::Value) -> String {
        serde_json::to_string(v).unwrap()
    }

    /// Every prefix of `json` must yield a prefix of the final script, and the
    /// yielded length must never shrink.
    fn check_all_prefixes(json: &str, script: &str) {
        let mut last = 0usize;
        let mut boundaries: Vec<usize> = (0..=json.len())
            .filter(|&i| json.is_char_boundary(i))
            .collect();
        boundaries.dedup();
        for i in boundaries {
            let got = execute_input_prefix(&json[..i]);
            if let Some(g) = &got {
                assert!(
                    script.starts_with(g.as_str()),
                    "prefix {i}: {g:?} is not a prefix of {script:?}"
                );
                assert!(g.len() >= last, "prefix {i}: shrank from {last}");
                last = g.len();
            } else {
                assert_eq!(last, 0, "prefix {i}: lost the value after having one");
            }
        }
        assert_eq!(execute_input_prefix(json).as_deref(), Some(script));
    }

    #[test]
    fn description_first() {
        let script = "const f = Insert(document, {type: \"frame\"});\nPrint(f)";
        let j = full(&json!({"description": "Add frame", "input": script}));
        check_all_prefixes(&j, script);
    }

    #[test]
    fn input_first() {
        let script = "Print(\"a\\\\b\")\n\tPrint('x')";
        let j = full(&json!({"input": script, "description": "Print"}));
        check_all_prefixes(&j, script);
    }

    #[test]
    fn with_uri_and_whitespace() {
        let script = "Print(1)";
        let j = "{ \"uri\" : \"C:\\\\a b\\\\x.pen\" ,\n  \"description\":\"d\",\r\n \"input\" :  \"Print(1)\" }";
        check_all_prefixes(j, script);
    }

    #[test]
    fn unicode_escapes_and_surrogates() {
        let script = "Print(\"caf\u{e9} \u{1F600} \u{4e2d} \\u0041\")";
        // serde writes non-ASCII as UTF-8; also build an all-escaped form.
        let j = full(&json!({"description": "x", "input": script}));
        check_all_prefixes(&j, script);
        let escaped = "{\"description\":\"x\",\"input\":\"Print(\\\"caf\\u00e9 \\ud83d\\ude00 \\u4E2D \\\\u0041\\\")\"}";
        check_all_prefixes(escaped, script);
    }

    #[test]
    fn cut_escapes_are_held_back() {
        let base = "{\"input\":\"ab";
        assert_eq!(
            execute_input_prefix(&format!("{base}\\")).as_deref(),
            Some("ab")
        );
        assert_eq!(
            execute_input_prefix(&format!("{base}\\u00")).as_deref(),
            Some("ab")
        );
        assert_eq!(
            execute_input_prefix(&format!("{base}\\ud83d")).as_deref(),
            Some("ab")
        );
        assert_eq!(
            execute_input_prefix(&format!("{base}\\ud83d\\")).as_deref(),
            Some("ab")
        );
        assert_eq!(
            execute_input_prefix(&format!("{base}\\ud83d\\ude0")).as_deref(),
            Some("ab")
        );
        assert_eq!(
            execute_input_prefix(&format!("{base}\\ud83d\\ude00")).as_deref(),
            Some("ab\u{1F600}")
        );
        // A lone high surrogate followed by a plain character is replaced.
        assert_eq!(
            execute_input_prefix(&format!("{base}\\ud83dx")).as_deref(),
            Some("ab\u{FFFD}x")
        );
        // Lone low surrogate.
        assert_eq!(
            execute_input_prefix(&format!("{base}\\ude00z\"}}")).as_deref(),
            Some("ab\u{FFFD}z")
        );
    }

    #[test]
    fn before_input_is_none() {
        for p in [
            "",
            "{",
            "{\"descr",
            "{\"description\":\"Add",
            "{\"description\":\"Add\",",
            "{\"description\":\"Add\",\"inp",
            "{\"description\":\"Add\",\"input\"",
            "{\"description\":\"Add\",\"input\":",
            "{\"description\":\"Add\",\"input\": ",
        ] {
            assert_eq!(execute_input_prefix(p), None, "{p:?}");
        }
        assert_eq!(
            execute_input_prefix("{\"description\":\"Add\",\"input\":\"").as_deref(),
            Some("")
        );
    }

    #[test]
    fn skips_nested_values_and_ignores_nested_input() {
        let j = "{\"edits\":[{\"find\":\"a\",\"input\":\"no\"}],\"meta\":{\"input\":\"no\",\"n\":[1,2,{\"x\":\"}\"}]},\"n\":-1.5e3,\"b\":true,\"z\":null,\"input\":\"yes\"}";
        check_all_prefixes(j, "yes");
    }

    #[test]
    fn non_string_or_missing_input() {
        assert_eq!(execute_input_prefix("{\"input\":null}"), None);
        assert_eq!(execute_input_prefix("{\"input\":12}"), None);
        assert_eq!(
            execute_input_prefix("{\"editId\":\"e1\",\"edits\":[]}"),
            None
        );
        assert_eq!(execute_input_prefix("[\"input\"]"), None);
        assert_eq!(execute_input_prefix("{\"a\":}"), None);
        assert_eq!(execute_input_prefix("{}"), None);
    }

    #[test]
    fn escaped_key_and_lookalike_keys() {
        assert_eq!(
            execute_input_prefix("{\"\\u0069nput\":\"ok").as_deref(),
            Some("ok")
        );
        assert_eq!(
            execute_input_prefix("{\"inputs\":\"no\",\"input_x\":\"no\",\"input\":\"yes\"")
                .as_deref(),
            Some("yes")
        );
    }

    #[test]
    fn simple_escapes() {
        let script = "a\"b\\c/d\u{8}\u{c}\n\r\te";
        let j = full(&json!({"input": script}));
        check_all_prefixes(&j, script);
        assert_eq!(
            execute_input_prefix("{\"input\":\"a\\/b\"}").as_deref(),
            Some("a/b")
        );
    }

    #[test]
    fn recorded_cli_chunks() {
        // Chunks exactly as Claude Code streamed them for an MCP `execute`.
        let chunks = [
            "",
            "{\"description\": \"Print a list",
            "\", \"input\": \"Print(\\\"line 1\\\");",
            "\\nPrint(\\\"line 2\\\");",
            "\"}",
        ];
        let mut acc = String::new();
        let mut seen = Vec::new();
        for c in chunks {
            acc.push_str(c);
            seen.push(execute_input_prefix(&acc));
        }
        assert_eq!(seen[1], None);
        assert_eq!(seen[2].as_deref(), Some("Print(\"line 1\");"));
        assert_eq!(
            seen[4].as_deref(),
            Some("Print(\"line 1\");\nPrint(\"line 2\");")
        );
    }

    #[test]
    fn large_script_every_split() {
        let script: String = (0..200)
            .map(|i| format!("Insert(document, {{name: \"n{i} \\u{{1F600}} é\"}});\n"))
            .collect();
        let j = full(&json!({"description": "Many", "input": script}));
        check_all_prefixes(&j, &script);
    }
}
