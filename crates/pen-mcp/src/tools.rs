//! Tool argument types (their JSON Schemas are generated with `schemars`) and
//! the static tool list advertised by `tools/list`.

use rmcp::model::{JsonObject, Tool, ToolAnnotations};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const GET_APP_STATE: &str = "get_app_state";
pub const EXECUTE: &str = "execute";
pub const SAVE: &str = "save";
pub const READ_SKILL: &str = "read_skill";
pub const START_TASK: &str = "start_task";
pub const GET_STYLE: &str = "get_style";
pub const IMPORT_WEBSITE: &str = "import_website";

const URI_DOC: &str = "Document to act on: a filesystem path or a file:// URI of a .pen file. \
Omit it to use the document focused in the editor.";

/// Arguments of `get_app_state`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GetAppStateArgs {
    #[schemars(description = URI_DOC)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
}

/// Arguments of `save`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SaveArgs {
    #[schemars(description = URI_DOC)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
}

/// One find/replace patch applied to the script of a failed `execute` call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CodeEdit {
    /// Exact text to look for in the previous script. Must occur exactly once
    /// unless `all` is true.
    pub find: String,
    /// Text that replaces the match.
    pub replace: String,
    /// Replace every occurrence instead of requiring a single match.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub all: bool,
}

/// Arguments of `execute`. Either `input` (a new script) or `editId` + `edits`
/// (a patch of the script of a failed call) must be given.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExecuteArgs {
    #[schemars(description = URI_DOC)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    /// Short imperative label of what this call does (3 to 8 words, e.g. "Add pricing
    /// section"). Shown to the user in the editor; write it first.
    pub description: String,
    /// JavaScript snippet run against the document with the editor API (see the
    /// `execute.md` skill). The whole call is atomic: it is applied as a single undo
    /// step, or not at all if it throws.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
    /// Id returned by a failed call. Send it with `edits` instead of `input` to fix
    /// that script without resending it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edit_id: Option<String>,
    /// Patches applied in order to the script identified by `editId`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edits: Vec<CodeEdit>,
}

impl ExecuteArgs {
    /// Checks the combinations the schema cannot express.
    pub fn validate(&self) -> Result<(), String> {
        if self.description.trim().is_empty() {
            return Err("`description` must not be empty".into());
        }
        match (&self.input, &self.edit_id) {
            (Some(_), Some(_)) => Err("pass either `input` or `editId` + `edits`, not both".into()),
            (None, None) => {
                Err("missing `input` (or `editId` + `edits` to patch a failed call)".into())
            }
            (Some(input), None) => {
                if !self.edits.is_empty() {
                    Err("`edits` requires `editId`".into())
                } else if input.trim().is_empty() {
                    Err("`input` must not be empty".into())
                } else {
                    Ok(())
                }
            }
            (None, Some(_)) => {
                if self.edits.is_empty() {
                    Err("`editId` requires at least one entry in `edits`".into())
                } else if self.edits.iter().any(|e| e.find.is_empty()) {
                    Err("`edits[].find` must not be empty".into())
                } else {
                    Ok(())
                }
            }
        }
    }
}

/// Arguments of `read_skill`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ReadSkillArgs {
    /// File to read, relative to the skills folder (e.g. `execute.md`,
    /// `pen-schema.md`). Omit it to read the index, `SKILL.md`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// Arguments of `start_task`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StartTaskArgs {
    /// Skill files to load, relative to the skills folder. Editing a document needs
    /// `execute.md` and `pen-schema.md`; `SKILL.md` holds the general guidance.
    pub skills: Vec<String>,
    /// Optional short title (2 to 5 words) describing the task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// Arguments of `get_style`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GetStyleArgs {
    #[schemars(description = URI_DOC)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    /// Style preset name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Preset parameters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Map<String, serde_json::Value>>,
}

/// Arguments of `import_website`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ImportWebsiteArgs {
    #[schemars(description = URI_DOC)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    /// Address of the page to import. `https://` is assumed when the scheme is
    /// missing (`http://` for localhost and IP addresses).
    pub url: String,
    /// Browser viewport width in CSS pixels (default 1440; 768 for a tablet
    /// layout, 390 for a phone).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    /// Browser viewport height in CSS pixels (default 900).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    /// Import the whole scrolling page (default true) or only what fits in
    /// the first viewport.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_page: Option<bool>,
    /// CSS selector of one element to import instead of the whole page
    /// (e.g. `main`, `#pricing`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
}

/// Smallest and largest viewport side accepted by `import_website`.
pub const IMPORT_MIN_SIDE: u32 = 200;
pub const IMPORT_MAX_SIDE: u32 = 4096;

impl ImportWebsiteArgs {
    /// Checks the arguments and returns the normalized URL.
    pub fn validate(&self) -> Result<String, String> {
        for (name, v) in [("width", self.width), ("height", self.height)] {
            if let Some(v) = v
                && !(IMPORT_MIN_SIDE..=IMPORT_MAX_SIDE).contains(&v)
            {
                return Err(format!(
                    "`{name}` must be between {IMPORT_MIN_SIDE} and {IMPORT_MAX_SIDE}"
                ));
            }
        }
        if self
            .selector
            .as_deref()
            .is_some_and(|s| s.trim().is_empty())
        {
            return Err("`selector` must not be empty (omit it to import the whole page)".into());
        }
        normalize_web_url(&self.url)
    }
}

/// Normalizes a web address typed by a user or an agent: trims it, adds
/// `https://` when the scheme is missing (`http://` for `localhost`, IPv4
/// and bracketed IPv6 hosts) and checks that the result is an `http(s)` URL
/// with a host. `file://` URLs pass as they are (local test pages).
pub fn normalize_web_url(input: &str) -> Result<String, String> {
    let s = input.trim();
    if s.is_empty() {
        return Err("enter the address of a web page".into());
    }
    if s.chars().any(char::is_whitespace) {
        return Err("a web address cannot contain spaces".into());
    }
    let has_scheme = s.split_once("://").is_some_and(|(scheme, _)| {
        scheme.starts_with(|c: char| c.is_ascii_alphabetic())
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
    });
    // `mailto:x`, `javascript:x`, `data:x`: a scheme without `//` (a port
    // after `host:` starts with a digit).
    if !has_scheme
        && let Some((head, rest)) = s.split_once(':')
        && head.starts_with(|c: char| c.is_ascii_alphabetic())
        && head.chars().all(|c| c.is_ascii_alphanumeric() || c == '+')
        && !rest.starts_with(|c: char| c.is_ascii_digit())
    {
        return Err(format!(
            "only http and https pages can be imported (not `{head}:`)"
        ));
    }
    let full = if has_scheme {
        s.to_string()
    } else if let Some(rest) = s.strip_prefix("//") {
        format!("https://{rest}")
    } else if is_local_host(s) {
        format!("http://{s}")
    } else {
        format!("https://{s}")
    };
    let parsed = url::Url::parse(&full).map_err(|e| format!("not a valid web address ({e})"))?;
    match parsed.scheme() {
        "http" | "https" => {}
        "file" => return Ok(parsed.to_string()),
        other => {
            return Err(format!(
                "only http and https pages can be imported (not `{other}:`)"
            ));
        }
    }
    let Some(host) = parsed.host_str().filter(|h| !h.is_empty()) else {
        return Err("the address has no host name".into());
    };
    // `example` alone is almost always a typo; local names need a scheme.
    if !has_scheme && !host.contains(['.', ':', '[']) && !host.eq_ignore_ascii_case("localhost") {
        return Err(format!("`{host}` is not a complete web address"));
    }
    Ok(parsed.to_string())
}

/// `localhost`, an IPv4 address or a bracketed IPv6 address, with an
/// optional port and path: these default to `http://`.
fn is_local_host(s: &str) -> bool {
    let end = s.find(['/', '?', '#']).unwrap_or(s.len());
    let authority = &s[..end];
    if let Some(rest) = authority.strip_prefix('[') {
        return rest
            .split_once(']')
            .is_some_and(|(_, port)| port.is_empty() || port.starts_with(':'));
    }
    let host = authority.split(':').next().unwrap_or("");
    host.eq_ignore_ascii_case("localhost") || host.parse::<std::net::Ipv4Addr>().is_ok()
}

/// What a backend inserted for an `import_website` call. [`Self::to_output`]
/// builds the tool result (shared by the GUI and headless backends).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ImportedPage {
    /// Id of the new top-level frame.
    pub frame_id: String,
    /// Page title (also the frame name).
    pub title: String,
    pub url: String,
    /// Width and height of the frame.
    pub size: (f32, f32),
    /// Short `label: value` facts about the import (nodes, images, time, ...).
    pub stats: Vec<(String, String)>,
    pub warnings: Vec<String>,
    /// Number of files written next to the document (images, fonts).
    pub assets: usize,
    /// PNG of the page as the browser rendered it.
    pub screenshot: Option<Vec<u8>>,
}

/// Warnings listed in the tool result; the rest is counted.
const MAX_WARNINGS: usize = 30;

impl ImportedPage {
    pub fn to_output(&self) -> crate::ToolOutput {
        let mut t = format!(
            "Imported {} as the new top-level frame `{}` (\"{}\", {} x {}).\n",
            self.url,
            self.frame_id,
            self.title,
            self.size.0.round(),
            self.size.1.round()
        );
        if !self.stats.is_empty() {
            t.push_str("\nStats:\n");
            for (k, v) in &self.stats {
                t.push_str(&format!("- {k}: {v}\n"));
            }
        }
        if self.assets > 0 {
            t.push_str(&format!(
                "\n{} asset file(s) were saved next to the document.\n",
                self.assets
            ));
        }
        if !self.warnings.is_empty() {
            t.push_str("\nWarnings:\n");
            for w in self.warnings.iter().take(MAX_WARNINGS) {
                t.push_str(&format!("- {w}\n"));
            }
            if self.warnings.len() > MAX_WARNINGS {
                t.push_str(&format!(
                    "- ... and {} more\n",
                    self.warnings.len() - MAX_WARNINGS
                ));
            }
        }
        t.push_str(
            "\nThe layers are absolutely positioned copies of the page's boxes and text. \
             Read them with `Get` (use a depth limit) and take a `TakeScreenshot` of the \
             frame before restyling it or rebuilding sections with auto layout.",
        );
        if self.screenshot.is_some() {
            t.push_str(" The image below is the original page as the browser rendered it.");
        }
        let mut out = crate::ToolOutput::text(t);
        if let Some(png) = &self.screenshot {
            out = out.with_image(png.clone());
        }
        out
    }
}

fn tool<T: JsonSchema + 'static>(
    name: &'static str,
    title: &str,
    description: &'static str,
    annotations: ToolAnnotations,
) -> Tool {
    Tool::new(name, description, JsonObject::new())
        .with_input_schema::<T>()
        .with_title(title)
        .with_annotations(annotations.open_world(false))
}

/// The tools served, in the order they are listed. `save` is omitted when
/// `include_save` is false.
pub fn tool_list(include_save: bool) -> Vec<Tool> {
    let read_only = || ToolAnnotations::new().read_only(true).idempotent(true);
    let mut tools = vec![
        tool::<StartTaskArgs>(
            START_TASK,
            "Start task",
            "Call this first when beginning a design task. Loads the requested skill files \
             (guides for the OpenPen scripting API and the .pen format) and returns them \
             concatenated. Request `execute.md` and `pen-schema.md` before editing.",
            read_only(),
        ),
        tool::<ReadSkillArgs>(
            READ_SKILL,
            "Read skill",
            "Reads one skill file from the OpenPen skills folder. Without `path` it returns \
             `SKILL.md`, which lists the other files.",
            read_only(),
        ),
        tool::<GetAppStateArgs>(
            GET_APP_STATE,
            "Get editor state",
            "Describes the editor: which document is focused (its URI), the selected nodes, \
             the top-level frames and the reusable components. Use it to orient yourself \
             before reading or changing a document.",
            ToolAnnotations::new().read_only(true),
        ),
        tool::<ExecuteArgs>(
            EXECUTE,
            "Execute script",
            "Reads or edits a .pen document by running a JavaScript snippet with the OpenPen \
             editor API (Insert, Update, Copy, Delete, Get, ...; see the `execute.md` skill). \
             The call is a single transaction: it becomes one undo step and is shown live on \
             the canvas, or nothing changes if it fails. A failed call returns an `editId`; \
             fix it by sending `editId` + `edits` instead of the whole script.",
            ToolAnnotations::new()
                .read_only(false)
                .destructive(true)
                .idempotent(false),
        ),
    ];
    if include_save {
        tools.push(tool::<SaveArgs>(
            SAVE,
            "Save document",
            "Writes the document to its file on disk, after pending work (such as images \
             still loading) has finished.",
            ToolAnnotations::new()
                .read_only(false)
                .destructive(false)
                .idempotent(true),
        ));
    }
    tools.push(
        tool::<ImportWebsiteArgs>(
            IMPORT_WEBSITE,
            "Import web page",
            "Loads a web page in a private headless browser (Edge or Chrome on this \
             computer), reads its rendered boxes, text and images, and adds it to the \
             document as one new top-level frame of editable layers, placed in free space. \
             Use it to recreate or restyle an existing site, or to start from a reference \
             page. Images and fonts are saved next to the .pen file, so the document must \
             have been saved once. Takes 10 to 90 seconds. Returns the new frame id, the \
             page title, warnings and a screenshot of the original page. One undo step.",
            ToolAnnotations::new(),
        )
        .with_annotations(
            ToolAnnotations::new()
                .read_only(false)
                .destructive(false)
                .idempotent(false)
                .open_world(true),
        ),
    );
    tools.push(tool::<GetStyleArgs>(
        GET_STYLE,
        "Get style preset",
        "Style presets are not available in this version of OpenPen; this tool only \
         returns a notice. Choose typography and colors yourself.",
        read_only(),
    ));
    tools
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exec(input: Option<&str>, edit_id: Option<&str>, edits: usize) -> ExecuteArgs {
        ExecuteArgs {
            uri: None,
            description: "Do it".into(),
            input: input.map(Into::into),
            edit_id: edit_id.map(Into::into),
            edits: (0..edits)
                .map(|_| CodeEdit {
                    find: "a".into(),
                    replace: "b".into(),
                    all: false,
                })
                .collect(),
        }
    }

    #[test]
    fn execute_validation() {
        assert!(exec(Some("Print(1)"), None, 0).validate().is_ok());
        assert!(exec(None, Some("e1"), 1).validate().is_ok());
        assert!(exec(None, None, 0).validate().is_err());
        assert!(exec(Some("x"), Some("e1"), 1).validate().is_err());
        assert!(exec(None, Some("e1"), 0).validate().is_err());
        assert!(exec(Some("x"), None, 1).validate().is_err());
        let mut no_desc = exec(Some("x"), None, 0);
        no_desc.description = " ".into();
        assert!(no_desc.validate().is_err());
    }

    #[test]
    fn web_urls_are_normalized() {
        let ok = |s: &str| normalize_web_url(s).unwrap();
        assert_eq!(ok("example.com"), "https://example.com/");
        assert_eq!(ok("  Example.com/a?b=1#c "), "https://example.com/a?b=1#c");
        assert_eq!(ok("http://example.com"), "http://example.com/");
        assert_eq!(ok("HTTPS://example.com/x"), "https://example.com/x");
        assert_eq!(ok("//cdn.example.com/p"), "https://cdn.example.com/p");
        assert_eq!(ok("localhost:3000/app"), "http://localhost:3000/app");
        assert_eq!(ok("127.0.0.1:8080"), "http://127.0.0.1:8080/");
        assert_eq!(ok("[::1]:5173/"), "http://[::1]:5173/");
        assert_eq!(ok("http://intranet"), "http://intranet/");
        assert_eq!(
            ok("file:///C:/site/index.html"),
            "file:///C:/site/index.html"
        );
        for bad in [
            "",
            "   ",
            "exa mple.com",
            "ftp://example.com",
            "javascript:alert(1)",
            "mailto:a@b.c",
            "example",
            "https://",
        ] {
            assert!(normalize_web_url(bad).is_err(), "{bad:?} should fail");
        }
    }

    #[test]
    fn import_args_validation_and_wire_names() {
        let a: ImportWebsiteArgs = serde_json::from_value(serde_json::json!({
            "url": "example.com", "width": 390, "fullPage": false, "selector": "main"
        }))
        .unwrap();
        assert_eq!(a.full_page, Some(false));
        assert_eq!(a.validate().unwrap(), "https://example.com/");
        let bad_w = ImportWebsiteArgs {
            width: Some(10),
            ..a.clone()
        };
        assert!(bad_w.validate().unwrap_err().contains("width"));
        let bad_sel = ImportWebsiteArgs {
            selector: Some(" ".into()),
            ..a.clone()
        };
        assert!(bad_sel.validate().is_err());
        let bad_url = ImportWebsiteArgs {
            url: "notaurl".into(),
            ..a
        };
        assert!(bad_url.validate().is_err());
    }

    #[test]
    fn imported_page_output() {
        let page = ImportedPage {
            frame_id: "f1".into(),
            title: "Example".into(),
            url: "https://example.com/".into(),
            size: (1440.0, 2000.4),
            stats: vec![("nodes".into(), "12".into())],
            warnings: (0..35).map(|i| format!("w{i}")).collect(),
            assets: 2,
            screenshot: Some(vec![1, 2, 3]),
        };
        let out = page.to_output();
        assert!(!out.is_error);
        assert!(out.text.contains("`f1`"));
        assert!(out.text.contains("1440 x 2000"));
        assert!(out.text.contains("- nodes: 12"));
        assert!(out.text.contains("w29") && !out.text.contains("w30\n"));
        assert!(out.text.contains("5 more"));
        assert_eq!(out.images, vec![vec![1, 2, 3]]);
    }

    #[test]
    fn import_tool_is_listed_as_open_world() {
        let t = tool_list(true)
            .into_iter()
            .find(|t| t.name == IMPORT_WEBSITE)
            .unwrap();
        let ann = t.annotations.as_ref().unwrap();
        assert_eq!(ann.open_world_hint, Some(true));
        assert_eq!(ann.read_only_hint, Some(false));
        let props = t.input_schema["properties"].as_object().unwrap();
        for key in ["uri", "url", "width", "height", "fullPage", "selector"] {
            assert!(props[key].get("description").is_some(), "{key}");
        }
        assert_eq!(t.input_schema["required"], serde_json::json!(["url"]));
    }

    #[test]
    fn execute_args_wire_names() {
        let v: ExecuteArgs = serde_json::from_value(serde_json::json!({
            "description": "Fix", "editId": "e7",
            "edits": [{"find": "x", "replace": "y", "all": true}]
        }))
        .unwrap();
        assert_eq!(v.edit_id.as_deref(), Some("e7"));
        assert!(v.edits[0].all);
    }

    #[test]
    fn schemas_are_objects_with_descriptions() {
        for t in tool_list(true) {
            assert_eq!(t.input_schema["type"], "object", "{}", t.name);
            assert!(t.description.as_ref().is_some_and(|d| d.len() > 20));
        }
        let exec = tool_list(false)
            .into_iter()
            .find(|t| t.name == EXECUTE)
            .unwrap();
        let props = exec.input_schema["properties"].as_object().unwrap();
        for key in ["uri", "description", "input", "editId", "edits"] {
            assert!(props.contains_key(key), "missing {key}");
            assert!(
                props[key].get("description").is_some(),
                "{key} has no description"
            );
        }
        assert_eq!(
            exec.input_schema["required"],
            serde_json::json!(["description"])
        );
        assert!(tool_list(false).iter().all(|t| t.name != SAVE));
    }
}
