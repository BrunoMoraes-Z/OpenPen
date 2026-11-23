//! Loading (parse -> migrate -> typed conversion -> repairs) and saving.

use std::collections::HashSet;
use std::io;

use serde_json::Value;

use crate::diag::{DiagCode, Diagnostic};
use crate::document::{CURRENT_VERSION, Document};
use crate::id::IdGenerator;
use crate::json::{strip_jsonc_comments, to_js_pretty};
use crate::migrate::{AppliedMigration, MigrateCx, MigrateError, migrate};
use crate::node::{Node, NodeKind};
use crate::object::{Cx, FromJson};

/// Fatal load error. Everything else is reported as a warning in [`LoadReport`].
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum LoadError {
    #[error("file is not valid UTF-8")]
    InvalidUtf8,
    #[error("invalid JSON at line {line}, column {column}: {message}")]
    Json {
        line: usize,
        column: usize,
        message: String,
    },
    #[error(transparent)]
    Migrate(#[from] MigrateError),
}

#[derive(Debug, Clone)]
pub struct LoadOptions {
    /// Apply the legacy load-time repairs: generate missing ids, regenerate
    /// duplicate ids, drop `placeholder` flags and self-referencing children.
    pub repair: bool,
    /// Seed for generated ids (`None` = random).
    pub id_seed: Option<u64>,
}

impl Default for LoadOptions {
    fn default() -> Self {
        LoadOptions {
            repair: true,
            id_seed: None,
        }
    }
}

/// Line terminator written by [`save_document_with`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineEnding {
    #[default]
    Lf,
    CrLf,
}

/// Text formatting of a saved file. The default matches the legacy app
/// (`JSON.stringify(doc, null, 2)`: LF, no trailing newline, no BOM);
/// [`SaveOptions::detect`] reproduces an existing file's conventions so that
/// load -> save of an unchanged document is byte-identical.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SaveOptions {
    pub line_ending: LineEnding,
    /// End the file with a line terminator.
    pub trailing_newline: bool,
    /// Start the file with a UTF-8 byte order mark.
    pub bom: bool,
}

impl SaveOptions {
    /// Conventions of the file `bytes` (the first line break decides the line ending).
    pub fn detect(bytes: &[u8]) -> SaveOptions {
        let (body, bom) = match bytes.strip_prefix(b"\xEF\xBB\xBF") {
            Some(rest) => (rest, true),
            None => (bytes, false),
        };
        let line_ending = match body.iter().position(|&b| b == b'\n') {
            Some(i) if i > 0 && body[i - 1] == b'\r' => LineEnding::CrLf,
            _ => LineEnding::Lf,
        };
        SaveOptions {
            line_ending,
            trailing_newline: body.ends_with(b"\n"),
            bom,
        }
    }
}

/// What happened while loading.
#[derive(Debug, Clone, Default)]
pub struct LoadReport {
    /// `version` found in the file (before migration).
    pub source_version: String,
    pub migrations: Vec<AppliedMigration>,
    /// Non-fatal problems: invalid values kept verbatim, repairs, migration notes.
    pub warnings: Vec<Diagnostic>,
    pub had_bom: bool,
    /// The file only parsed after stripping `//` / `/* */` comments.
    pub had_comments: bool,
    /// Empty input, loaded as an empty document.
    pub empty_input: bool,
    /// Line ending / trailing newline / BOM of the input, for saving it back unchanged.
    pub save_options: SaveOptions,
}

impl LoadReport {
    pub fn migrated(&self) -> bool {
        !self.migrations.is_empty()
    }
}

/// Parses bytes as (JSONC-tolerant) JSON. Returns the value plus
/// `(had_bom, had_comments, empty)`.
pub fn parse_json(bytes: &[u8]) -> Result<(Value, bool, bool, bool), LoadError> {
    let (bytes, had_bom) = match bytes.strip_prefix(b"\xEF\xBB\xBF") {
        Some(rest) => (rest, true),
        None => (bytes, false),
    };
    if bytes.iter().all(u8::is_ascii_whitespace) {
        let empty = serde_json::json!({ "version": CURRENT_VERSION, "children": [] });
        return Ok((empty, had_bom, false, true));
    }
    match serde_json::from_slice::<Value>(bytes) {
        Ok(v) => Ok((v, had_bom, false, false)),
        Err(first) => {
            let text = std::str::from_utf8(bytes).map_err(|_| LoadError::InvalidUtf8)?;
            let stripped = strip_jsonc_comments(text);
            if stripped.len() == text.len() && stripped == text {
                return Err(json_error(&first));
            }
            serde_json::from_str::<Value>(&stripped)
                .map(|v| (v, had_bom, true, false))
                .map_err(|_| json_error(&first))
        }
    }
}

fn json_error(e: &serde_json::Error) -> LoadError {
    LoadError::Json {
        line: e.line(),
        column: e.column(),
        message: e.to_string(),
    }
}

/// Loads a `.pen` file with the default options.
pub fn load_document(bytes: &[u8]) -> Result<(Document, LoadReport), LoadError> {
    load_document_with(bytes, &LoadOptions::default())
}

pub fn load_document_with(
    bytes: &[u8],
    opts: &LoadOptions,
) -> Result<(Document, LoadReport), LoadError> {
    let (value, had_bom, had_comments, empty_input) = parse_json(bytes)?;
    let (doc, mut report) = load_value_with(value, opts)?;
    report.had_bom = had_bom;
    report.had_comments = had_comments;
    report.empty_input = empty_input;
    if !empty_input {
        report.save_options = SaveOptions::detect(bytes);
    }
    Ok((doc, report))
}

/// Migrates and converts an already parsed JSON value.
pub fn load_value_with(
    mut value: Value,
    opts: &LoadOptions,
) -> Result<(Document, LoadReport), LoadError> {
    let mut report = LoadReport {
        source_version: value
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        ..Default::default()
    };
    let ids = opts.id_seed.map(IdGenerator::with_seed).unwrap_or_default();
    let mut mcx = MigrateCx {
        ids,
        warnings: Vec::new(),
    };
    report.migrations = migrate(&mut value, &mut mcx)?;
    report.warnings.append(&mut mcx.warnings);

    let (mut doc, mut diags) = document_from_value(value);
    report.warnings.append(&mut diags);
    if opts.repair {
        repair(&mut doc, &mut mcx.ids, &mut report.warnings);
    }
    Ok((doc, report))
}

/// Typed conversion of a current-version document value (no migration).
/// Invalid values are kept in `extra` and reported.
pub fn document_from_value(value: Value) -> (Document, Vec<Diagnostic>) {
    let mut cx = Cx::default();
    let doc = if Document::accepts(&value) {
        Document::convert(value, &mut cx)
    } else {
        Document::default()
    };
    cx.flush_issues(0, "");
    (doc, cx.diags)
}

/// Serializes like the legacy app (`JSON.stringify(doc, null, 2)`, no trailing newline).
pub fn save_document(doc: &Document) -> String {
    // The serializer only writes valid UTF-8.
    String::from_utf8(to_js_pretty(doc)).unwrap_or_default()
}

pub fn save_document_to<W: io::Write>(doc: &Document, mut w: W) -> io::Result<()> {
    w.write_all(&to_js_pretty(doc))
}

/// Serializes with explicit text conventions (see [`SaveOptions::detect`]).
pub fn save_document_with(doc: &Document, opts: &SaveOptions) -> String {
    let json = save_document(doc);
    let nl = match opts.line_ending {
        LineEnding::Lf => "\n",
        LineEnding::CrLf => "\r\n",
    };
    let mut out = String::with_capacity(json.len() + json.len() / 16 + 8);
    if opts.bom {
        out.push('\u{feff}');
    }
    // The serializer escapes line breaks inside strings, so every raw `\n` is a line break.
    match opts.line_ending {
        LineEnding::Lf => out.push_str(&json),
        LineEnding::CrLf => out.push_str(&json.replace('\n', nl)),
    }
    if opts.trailing_newline {
        out.push_str(nl);
    }
    out
}

/// The document as a JSON value (numbers as they would be written).
pub fn document_to_value(doc: &Document) -> Value {
    serde_json::from_slice(&to_js_pretty(doc)).unwrap_or(Value::Null)
}

/// Visits tree nodes (`children`, recursively) and typed replacement nodes
/// inside `descendants`. The flag tells whether the node is inside an override.
fn visit_mut(node: &mut Node, in_override: bool, f: &mut dyn FnMut(&mut Node, bool)) {
    f(node, in_override);
    if let Some(children) = &mut node.children {
        for c in children {
            visit_mut(c, in_override, f);
        }
    }
    if let Some(desc) = &mut node.descendants {
        for d in desc.values_mut() {
            if d.is_typed() {
                visit_mut(d, true, f);
            } else if let Some(children) = &mut d.children {
                for c in children {
                    visit_mut(c, true, f);
                }
            }
        }
    }
}

/// Legacy load-time repairs.
fn repair(doc: &mut Document, ids: &mut IdGenerator, warnings: &mut Vec<Diagnostic>) {
    let Some(children) = &mut doc.children else {
        return;
    };

    let mut all: HashSet<String> = HashSet::new();
    for c in children.iter_mut() {
        visit_mut(c, false, &mut |n, _| {
            if let Some(id) = &n.id {
                all.insert(id.clone());
            }
        });
    }

    let mut seen: HashSet<String> = HashSet::with_capacity(all.len());
    let mut fresh = |all: &mut HashSet<String>| {
        let id = ids.next_unique(|c| all.contains(c));
        all.insert(id.clone());
        id
    };
    for c in children.iter_mut() {
        visit_mut(c, false, &mut |n, in_override| {
            if !n.is_typed() {
                return;
            }
            match &n.id {
                None => {
                    let id = fresh(&mut all);
                    warnings.push(Diagnostic::info(
                        DiagCode::Repaired,
                        &id,
                        "missing id generated",
                    ));
                    n.id = Some(id);
                }
                Some(id) if !in_override && !seen.insert(id.clone()) => {
                    let new = fresh(&mut all);
                    warnings.push(Diagnostic::warning(
                        DiagCode::Repaired,
                        id.as_str(),
                        format!("duplicate id regenerated as {new:?}"),
                    ));
                    seen.insert(new.clone());
                    n.id = Some(new);
                }
                _ => {}
            }
            if n.placeholder.is_some() && matches!(n.kind, Some(NodeKind::Frame | NodeKind::Ref)) {
                n.placeholder = None;
                warnings.push(Diagnostic::info(
                    DiagCode::Repaired,
                    n.id_str(),
                    "placeholder flag dropped",
                ));
            }
            let self_ref = n
                .children_slice()
                .first()
                .is_some_and(|c| c.is_ref() && c.id.is_some() && c.id == c.reference);
            if self_ref {
                n.children = Some(Vec::new());
                warnings.push(Diagnostic::warning(
                    DiagCode::Repaired,
                    n.id_str(),
                    "children dropped: first child is a self-referencing ref",
                ));
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::json_eq;

    #[test]
    fn empty_bom_comments_crlf() {
        let (doc, r) = load_document(b"").unwrap();
        assert!(r.empty_input);
        assert_eq!(
            save_document(&doc),
            "{\n  \"version\": \"2.22\",\n  \"children\": []\n}"
        );

        let src = "\u{FEFF}{\r\n  // comment\r\n  \"version\": \"2.22\", /* x */\r\n  \"children\": []\r\n}";
        let (doc, r) = load_document(src.as_bytes()).unwrap();
        assert!(r.had_bom && r.had_comments);
        assert_eq!(doc.version.as_deref(), Some("2.22"));
    }

    #[test]
    fn errors() {
        assert!(matches!(
            load_document(b"{ nope"),
            Err(LoadError::Json { .. })
        ));
        assert!(matches!(
            load_document(br#"{"version": "9.0", "children": []}"#),
            Err(LoadError::Migrate(MigrateError::UnsupportedVersion(_)))
        ));
        assert!(matches!(
            load_document(b"[1]"),
            Err(LoadError::Migrate(MigrateError::NotAnObject))
        ));
    }

    #[test]
    fn roundtrip_is_lossless() {
        let src = r#"{
  "version": "2.22",
  "children": [
    {
      "id": "Ab12c",
      "type": "ref",
      "ref": "Comp1",
      "futureProp": {
        "a": 1
      },
      "width": "fill_container(12.5)"
    },
    {
      "type": "frame",
      "id": "Comp1",
      "reusable": true,
      "width": "nonsense",
      "children": []
    }
  ],
  "futureTop": [
    1,
    2
  ]
}"#;
        let (doc, report) = load_document(src.as_bytes()).unwrap();
        assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
        assert_eq!(report.warnings[0].code, DiagCode::InvalidSizing);
        assert_eq!(save_document(&doc), src);
    }

    #[test]
    fn repairs_ids_and_placeholders() {
        let src = br#"{"version": "2.22", "children": [
            {"type": "frame", "id": "a", "placeholder": true, "children": [{"type": "rectangle"}, {"type": "text", "id": "a"}]}
        ]}"#;
        let (doc, report) = load_document_with(
            src,
            &LoadOptions {
                repair: true,
                id_seed: Some(3),
            },
        )
        .unwrap();
        let f = &doc.children_slice()[0];
        assert!(f.placeholder.is_none());
        let kids = f.children_slice();
        assert!(kids[0].id.is_some());
        assert_ne!(kids[1].id.as_deref(), Some("a"));
        assert_eq!(report.warnings.len(), 3, "{:?}", report.warnings);

        let (doc, _) = load_document_with(
            src,
            &LoadOptions {
                repair: false,
                id_seed: None,
            },
        )
        .unwrap();
        let expected: Value = serde_json::from_slice(src).unwrap();
        assert!(json_eq(&document_to_value(&doc), &expected));
    }

    #[test]
    fn old_document_is_migrated() {
        let src = br#"{"version": "2.6", "children": [{"type": "icon_font", "id": "i1", "iconFontName": "x", "iconFontFamily": "lucide", "width": 10, "height": 10}]}"#;
        let (doc, report) = load_document(src).unwrap();
        assert_eq!(report.source_version, "2.6");
        assert_eq!(report.migrations.len(), 16);
        let n = &doc.children_slice()[0];
        assert_eq!(n.kind, Some(NodeKind::Icon));
        assert_eq!(n.icon.as_ref().map(|s| s.raw()), Some("x"));
    }
}

#[cfg(test)]
mod save_tests {
    use super::*;

    #[test]
    fn detects_and_reproduces_line_endings() {
        let lf = b"{\n  \"version\": \"2.22\",\n  \"children\": []\n}";
        let crlf = b"{\r\n  \"version\": \"2.22\",\r\n  \"children\": []\r\n}\r\n";
        let bom_lf = b"\xEF\xBB\xBF{\n  \"version\": \"2.22\",\n  \"children\": []\n}\n";
        for src in [&lf[..], &crlf[..], &bom_lf[..]] {
            let (doc, report) = load_document(src).unwrap();
            assert_eq!(report.save_options, SaveOptions::detect(src));
            let out = save_document_with(&doc, &report.save_options);
            assert_eq!(out.as_bytes(), src, "{:?}", report.save_options);
        }
        assert_eq!(SaveOptions::detect(lf), SaveOptions::default());
        let o = SaveOptions::detect(crlf);
        assert_eq!(o.line_ending, LineEnding::CrLf);
        assert!(o.trailing_newline && !o.bom);
    }

    #[test]
    fn crlf_never_touches_string_contents() {
        let src = b"{\r\n  \"version\": \"2.22\",\r\n  \"children\": [\r\n    {\r\n      \"type\": \"text\",\r\n      \"id\": \"t\",\r\n      \"content\": \"a\\nb\"\r\n    }\r\n  ]\r\n}";
        let (doc, report) = load_document(src).unwrap();
        let out = save_document_with(&doc, &report.save_options);
        assert_eq!(out.as_bytes(), &src[..]);
    }
}
