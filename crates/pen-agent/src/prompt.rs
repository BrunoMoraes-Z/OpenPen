//! Prompt pieces of the embedded chat.
//!
//! The system prompt is the skill index (`SKILL.md`) plus a short rule about
//! the final message. The detailed guides (`execute.md`, `pen-schema.md`) are
//! loaded on demand with `start_task` / `read_skill`, so they stay out of the
//! prompt. The editor state is volatile, so it goes into the first user
//! message instead of the system prompt (which must stay byte-identical for
//! prompt caching).

use std::path::Path;

/// How the chat should end its turns. The user sees the canvas change live,
/// so a recap of the edits is noise.
pub const FINAL_MESSAGE_RULE: &str = "\
<final_message>
You are running inside the OpenPen editor's chat panel. The user watches every \
`execute` call change the canvas while you work, so do not recap what you built \
or list the changes. Finish with at most one short sentence (or nothing), unless \
the user asked a question that needs an answer or you need a decision from them.
</final_message>";

/// Preamble of the chat system prompt, before the skill index.
const CHAT_PREAMBLE: &str = "\
You are the design assistant embedded in OpenPen, a native editor for .pen design \
files. You read and change the user's documents only through the OpenPen tools \
(`start_task`, `read_skill`, `get_app_state`, `execute`, `save`, \
`import_website`). Begin a design task with `start_task` to load `execute.md` and \
`pen-schema.md`.";

/// Builds the system prompt from the contents of `SKILL.md`.
pub fn chat_system_prompt(skill_index: &str) -> String {
    let skill = strip_front_matter(skill_index).trim();
    format!(
        "{CHAT_PREAMBLE}\n\n<skill name=\"openpen\">\n{skill}\n</skill>\n\n{FINAL_MESSAGE_RULE}"
    )
}

/// Reads `SKILL.md` from a skills folder (see `pen_mcp::find_skills_dir`).
pub fn load_skill_index(skills_dir: &Path) -> Result<String, String> {
    pen_mcp::SkillStore::new(skills_dir)
        .read(None)
        .map_err(|e| e.to_string())
}

/// Wraps a prefetched `get_app_state` result for the first user message.
pub fn app_state_context(app_state: &str) -> String {
    format!(
        "<editor_state>\n{}\n</editor_state>\nThis is the current editor state, fetched \
         just now; it is not stale, so you do not need to call `get_app_state` before \
         starting.",
        app_state.trim()
    )
}

fn strip_front_matter(s: &str) -> &str {
    let t = s.trim_start_matches('\u{feff}');
    let Some(rest) = t.strip_prefix("---") else {
        return t;
    };
    match rest.find("\n---") {
        Some(end) => {
            let after = &rest[end + 4..];
            after.strip_prefix('\r').unwrap_or(after)
        }
        None => t,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_prompt_has_skill_and_rule() {
        let p = chat_system_prompt("---\nname: openpen\ndescription: x\n---\n\n# Working\nBody");
        assert!(p.contains("<skill name=\"openpen\">\n# Working\nBody\n</skill>"));
        assert!(p.ends_with("</final_message>"));
        assert!(!p.contains("description: x"));
        // Deterministic: the prompt is part of the cached prefix.
        assert_eq!(
            p,
            chat_system_prompt("---\nname: openpen\ndescription: x\n---\n\n# Working\nBody")
        );
    }

    #[test]
    fn no_front_matter() {
        assert!(chat_system_prompt("# Only").contains("\n# Only\n"));
    }

    #[test]
    fn repo_skill_index_loads() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/openpen");
        if dir.exists() {
            let s = load_skill_index(&dir).unwrap();
            assert!(chat_system_prompt(&s).contains("execute.md"));
        }
    }

    #[test]
    fn app_state_wrapper() {
        let c = app_state_context("  Focused: a.pen\n");
        assert!(c.starts_with("<editor_state>\nFocused: a.pen\n</editor_state>"));
    }
}
