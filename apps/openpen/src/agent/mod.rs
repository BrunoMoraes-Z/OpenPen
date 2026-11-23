//! External AI agents (Claude Code, Codex, ...) driving the running app
//! through MCP: the GUI [`backend`] (`pen_mcp::ToolBackend`), the foreground
//! [`hub`] that runs the calls on the right document, the live canvas
//! feedback ([`fx`] timing, [`paint`] drawing) and the connect dialog
//! ([`ui`]). See docs/agent-integration.md.

pub mod backend;
pub mod fx;
pub mod hub;
pub mod paint;
pub mod route;
pub mod stream;
pub mod ui;

use std::collections::HashMap;

use pen_doc::NodeKey;

use crate::scene_document::SceneDocument;
use fx::TouchedNode;

/// A connected agent (one MCP session).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentInfo {
    pub client_id: String,
    pub name: String,
    /// RGB.
    pub color: u32,
}

/// Rendering services for `TakeScreenshot` / `Export`: the CPU defaults
/// (pen-raster) render exactly what the canvas shows, without the overlay.
#[derive(Debug, Default, Clone, Copy)]
pub struct AppScriptHost;

impl pen_script::ScriptHost for AppScriptHost {}

/// Resolves the nodes a script touched to world bounds, for the flashes:
/// one entry per node (a write wins over a read of the same node), nodes
/// whose ancestor is also listed with the same kind of access are dropped
/// (a new frame flashes once, not once per child), and unknown ids skipped.
pub fn touched_nodes(doc: &SceneDocument, touched: &[pen_script::Touch]) -> Vec<TouchedNode> {
    let mut order: Vec<NodeKey> = Vec::new();
    let mut write: HashMap<NodeKey, bool> = HashMap::new();
    let mut ids: HashMap<NodeKey, &str> = HashMap::new();
    for t in touched {
        let Some(k) = doc.scene.find(&t.id) else {
            continue;
        };
        match write.get_mut(&k) {
            Some(w) => *w |= t.write,
            None => {
                write.insert(k, t.write);
                ids.insert(k, &t.id);
                order.push(k);
            }
        }
    }
    let covered = |k: NodeKey| {
        let w = write[&k];
        let mut p = doc.get(k).and_then(|n| n.parent);
        while let Some(pk) = p {
            if write.get(&pk) == Some(&w) {
                return true;
            }
            p = doc.get(pk).and_then(|n| n.parent);
        }
        false
    };
    order
        .into_iter()
        .filter(|&k| !covered(k))
        .filter_map(|k| {
            Some(TouchedNode {
                id: ids[&k].to_string(),
                rect: doc.world_aabb(k)?,
                write: write[&k],
            })
        })
        .collect()
}

/// Agent color for the `n`-th connection.
pub fn color_for(n: usize) -> u32 {
    fx::AGENT_COLORS[n % fx::AGENT_COLORS.len()]
}
