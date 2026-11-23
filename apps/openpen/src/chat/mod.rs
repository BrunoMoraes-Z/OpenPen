//! The embedded chat (docs/chat.md): a panel in the left sidebar's Chat tab
//! that talks to an agent through `pen-agent` (Claude Code CLI or the
//! Anthropic API) and edits the panel's document live.
//!
//! - [`model`]: pure reducer from agent events to the panel's items;
//! - [`settings`]: engine settings (`chat-settings.json`) and the
//!   `openpen-cli` lookup;
//! - [`store`]: one JSON file per conversation (`chats/`);
//! - [`engine`]: the `pen-agent` session config;
//! - [`panel`]: the GPUI view.

pub mod engine;
pub mod model;
pub mod panel;
pub mod settings;
pub mod store;
