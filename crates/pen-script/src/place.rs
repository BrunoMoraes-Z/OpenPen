//! Inserting a ready-made node tree (e.g. a web page converted to `.pen`
//! JSON) at a free spot of the root, through the regular `execute` path: ids
//! are generated, the insert is one undo step and the host flashes the
//! touched nodes like any agent edit.

use serde_json::Value;

use crate::{ExecuteRequest, ExecuteResponse};

/// Gap between the new node and the existing top-level nodes.
pub const PLACE_PADDING: f32 = 100.0;

/// Marker printed before the id of the inserted node.
const ID_MARKER: &str = "__openpen_inserted__:";

/// Builds an `execute` request that inserts `node` (a `.pen` node object
/// without ids; `children` nested) at the root, in a free spot found with
/// `FindEmptySpace` to the right of the existing content. The node's own
/// `x`/`y` are replaced. Read the new id with [`inserted_id`].
pub fn insert_at_free_spot(
    agent_id: impl Into<String>,
    description: impl Into<String>,
    node: &Value,
) -> ExecuteRequest {
    // The JSON text becomes a JS string literal for `JSON.parse` (faster to
    // parse than an object literal, and safe: a JSON string is a valid JS
    // string literal).
    let json = serde_json::to_string(node).unwrap_or_else(|_| "{}".into());
    let literal = serde_json::to_string(&json).unwrap_or_else(|_| "\"{}\"".into());
    let input = format!(
        "const node = JSON.parse({literal});\n\
         const num = (v, d) => (typeof v === \"number\" && isFinite(v) && v > 0 ? v : d);\n\
         const at = FindEmptySpace({{width: num(node.width, 1440), height: num(node.height, 900), \
         padding: {PLACE_PADDING}}});\n\
         node.x = at.x;\n\
         node.y = at.y;\n\
         Print(\"{ID_MARKER}\" + Insert(document, node));\n"
    );
    ExecuteRequest {
        agent_id: agent_id.into(),
        description: description.into(),
        input: Some(input),
        edit_id: None,
        edits: Vec::new(),
    }
}

/// The id of the node inserted by a request from [`insert_at_free_spot`].
pub fn inserted_id(resp: &ExecuteResponse) -> Option<String> {
    if !resp.ok {
        return None;
    }
    resp.prints
        .iter()
        .find_map(|l| l.strip_prefix(ID_MARKER))
        .map(str::to_string)
}

/// Error text of a failed insert (the script's own position is noise here).
pub fn insert_error(resp: &ExecuteResponse) -> String {
    resp.error
        .as_ref()
        .map(|e| e.message.clone())
        .unwrap_or_else(|| resp.text.clone())
}
