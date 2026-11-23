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
