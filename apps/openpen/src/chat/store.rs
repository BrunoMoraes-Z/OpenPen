//! Conversation persistence: one JSON file per conversation in
//! `<data dir>/chats/`, named `<doc key>-<conversation id>.json`. The doc key
//! is a hash of the document's canonical path, so the conversations of a
//! document are found by listing the folder, without reading every file.
//! The newest one is reopened with the document. The Claude Code session id
//! is stored too, for `--resume`.
//!
//! Files that fail to parse are skipped (never fatal); writes are atomic.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::model::Item;
use super::settings::EngineKind;

pub const DIR_NAME: &str = "chats";
const VERSION: u32 = 1;

/// One saved conversation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredConversation {
    pub version: u32,
    /// `<ms since epoch, 13 digits>-<random hex>`: sorts by creation time.
    pub id: String,
    /// Canonical path of the document it belongs to.
    pub doc: String,
    pub title: String,
    pub engine: EngineKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Claude Code session id (`--resume`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub created_ms: u64,
    pub updated_ms: u64,
    pub items: Vec<Item>,
}

impl StoredConversation {
    pub fn new(id: String, doc: &Path, engine: EngineKind) -> Self {
        let now = now_ms();
        StoredConversation {
            version: VERSION,
            id,
            doc: doc_string(doc),
            title: String::new(),
            engine,
            model: None,
            session_id: None,
            created_ms: now,
            updated_ms: now,
            items: Vec::new(),
        }
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// A new conversation id (sortable by time).
pub fn new_id() -> String {
    let n = now_ms();
    let r = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0)
        ^ std::process::id().rotate_left(16);
    format!("{n:013}-{r:08x}")
}

fn doc_string(doc: &Path) -> String {
    doc.to_string_lossy().into_owned()
}

/// 16 hex chars of FNV-1a over the path (case-insensitive on Windows, where
/// paths are).
pub fn doc_key(doc: &Path) -> String {
    let s = doc_string(doc);
    let s = if cfg!(windows) {
        s.replace('/', "\\").to_lowercase()
    } else {
        s
    };
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

pub fn chats_dir(data_dir: &Path) -> PathBuf {
    data_dir.join(DIR_NAME)
}

fn file_for(dir: &Path, conv: &StoredConversation) -> PathBuf {
    dir.join(format!(
        "{}-{}.json",
        doc_key(Path::new(&conv.doc)),
        conv.id
    ))
}

/// Writes a conversation (creates the folder).
pub fn save(dir: &Path, conv: &StoredConversation) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let json = serde_json::to_string(conv).map_err(std::io::Error::other)?;
    crate::save::write_atomic(&file_for(dir, conv), json.as_bytes())
}

/// The conversations of `doc`, newest first (unreadable files skipped).
pub fn list_for_doc(dir: &Path, doc: &Path) -> Vec<StoredConversation> {
    let prefix = format!("{}-", doc_key(doc));
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension().is_some_and(|e| e == "json")
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(&prefix))
        })
        .collect();
    // Ids start with the creation time: the name order is the time order.
    names.sort();
    names.reverse();
    names
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .filter_map(|t| serde_json::from_str::<StoredConversation>(&t).ok())
        .collect()
}

/// The newest conversation of `doc`.
pub fn latest_for_doc(dir: &Path, doc: &Path) -> Option<StoredConversation> {
    list_for_doc(dir, doc).into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::super::model::{ToolCall, ToolState};
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("openpen-chat-store-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn round_trip_and_latest_per_document() {
        let dir = scratch("rt");
        let doc_a = Path::new("C:\\designs\\a.pen");
        let doc_b = Path::new("C:\\designs\\b.pen");
        assert!(latest_for_doc(&dir, doc_a).is_none(), "no folder yet");
        let mut c1 =
            StoredConversation::new("0000000000001-aaaa".into(), doc_a, EngineKind::ClaudeCode);
        c1.title = "First".into();
        c1.session_id = Some("sess-1".into());
        c1.model = Some("haiku".into());
        c1.items = vec![
            Item::User {
                text: "Build a card".into(),
                steer: false,
            },
            Item::Tool(ToolCall {
                id: "toolu_1".into(),
                name: "execute".into(),
                description: Some("Create card".into()),
                code: "x = 1".into(),
                state: ToolState::Done,
                result: Some("Done".into()),
                images: 0,
            }),
            Item::Text {
                text: "**Done.**".into(),
            },
        ];
        save(&dir, &c1).unwrap();
        let c2 =
            StoredConversation::new("0000000000002-bbbb".into(), doc_a, EngineKind::AnthropicApi);
        save(&dir, &c2).unwrap();
        let other =
            StoredConversation::new("0000000000003-cccc".into(), doc_b, EngineKind::ClaudeCode);
        save(&dir, &other).unwrap();
        // A broken file of the same document is skipped.
        std::fs::write(
            dir.join(format!("{}-0000000000004-dddd.json", doc_key(doc_a))),
            "{broken",
        )
        .unwrap();
        let list = list_for_doc(&dir, doc_a);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0], c2, "newest first");
        assert_eq!(list[1], c1, "lossless");
        assert_eq!(latest_for_doc(&dir, doc_b).unwrap().id, other.id);
        // Saving again replaces the file (no duplicate).
        let mut c2b = c2.clone();
        c2b.title = "Renamed".into();
        save(&dir, &c2b).unwrap();
        assert_eq!(list_for_doc(&dir, doc_a)[0].title, "Renamed");
        assert_eq!(list_for_doc(&dir, doc_a).len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn keys_and_ids() {
        if cfg!(windows) {
            assert_eq!(
                doc_key(Path::new("C:\\A\\b.pen")),
                doc_key(Path::new("c:/a/B.pen"))
            );
        }
        assert_ne!(doc_key(Path::new("/a.pen")), doc_key(Path::new("/b.pen")));
        assert_eq!(doc_key(Path::new("/a.pen")).len(), 16);
        let id = new_id();
        assert_eq!(id.len(), 13 + 1 + 8);
        assert!(id[..13].chars().all(|c| c.is_ascii_digit()));
    }
}
