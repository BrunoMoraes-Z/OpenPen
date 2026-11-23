//! Builds the `pen-agent` session of a chat panel from the settings: the
//! Claude Code engine (through `openpen-cli mcp` and the app's socket) or the
//! API engine (tools in-process through the hub's `GuiBackend`, with the
//! streaming hooks). No GPUI.

use std::path::PathBuf;
use std::sync::Arc;

use pen_agent::api::ApiConfig;
use pen_agent::cli::CliConfig;
use pen_agent::{BackendExecutor, EngineConfig, SessionConfig, execute_input_prefix};
use pen_mcp::CallCtx;
use pen_mcp::socket::DEFAULT_SOCKET_NAME;

use super::model::EXECUTE;
use super::settings::{ChatSettings, EngineKind};
use crate::agent::backend::GuiBackend;

/// Label of the chat's cursor on the canvas.
pub const CHAT_LABEL: &str = "OpenPen Chat";

/// Everything a session needs besides the settings.
pub struct EngineEnv {
    pub backend: GuiBackend,
    pub skills_dir: Option<PathBuf>,
    /// `openpen-cli` (Claude Code engine).
    pub cli_path: Option<PathBuf>,
    /// Socket the app's MCP server listens on (`None`: not running).
    pub socket: Option<String>,
    /// Document of the chat (`uri` of its tool calls and streams; `None` for
    /// an unsaved document: the focused one).
    pub doc_uri: Option<String>,
    /// In-process agent id (API engine).
    pub agent_id: String,
    /// Claude Code session to resume.
    pub resume: Option<String>,
    /// API engine overrides (tests): base URL and key. `None`: the default
    /// endpoint and the stored key (else `ANTHROPIC_API_KEY`).
    pub api_base_url: Option<String>,
    pub api_key: Option<pen_agent::key::ApiKey>,
}

/// The system prompt: `SKILL.md` of the skills folder, or the bare preamble
/// when the skills are not installed.
pub fn system_prompt(skills_dir: Option<&std::path::Path>) -> String {
    let index = skills_dir
        .and_then(|d| pen_agent::prompt::load_skill_index(d).ok())
        .unwrap_or_default();
    pen_agent::prompt::chat_system_prompt(&index)
}

/// The session config for `settings`. Errors are user-facing.
pub fn session_config(settings: &ChatSettings, env: &EngineEnv) -> Result<SessionConfig, String> {
    let engine = match settings.engine {
        EngineKind::ClaudeCode => {
            let Some(cli_path) = env.cli_path.clone() else {
                return Err(
                    "openpen-cli was not found next to OpenPen, in the cargo target folder or on PATH. Build it with `cargo build -p openpen-cli`."
                        .into(),
                );
            };
            let Some(socket) = env.socket.as_deref() else {
                return Err(
                    "OpenPen's MCP server is not running (another OpenPen window may own the socket), so Claude Code cannot reach this document. Close the other window or use the Anthropic API engine."
                        .into(),
                );
            };
            let mut cli = CliConfig::new(cli_path);
            cli.mcp_socket = (socket != DEFAULT_SOCKET_NAME).then(|| socket.to_string());
            cli.agent_name = CHAT_LABEL.into();
            cli.allow_web = settings.allow_web;
            cli.extra_args = settings.cli_extra_args();
            EngineConfig::ClaudeCli(cli)
        }
        EngineKind::AnthropicApi => {
            let ctx = CallCtx {
                client_id: env.agent_id.clone(),
                agent_name: CHAT_LABEL.into(),
                ..Default::default()
            };
            let (partial, abandon) = (env.backend.clone(), env.backend.clone());
            let (agent_p, agent_a) = (env.agent_id.clone(), env.agent_id.clone());
            let uri = env.doc_uri.clone();
            let exec =
                BackendExecutor::new(Arc::new(env.backend.clone()), env.skills_dir.clone(), ctx)
                    .with_partial_hook(move |tool_use_id, name, json| {
                        if name == EXECUTE
                            && let Some(prefix) = execute_input_prefix(json)
                        {
                            partial.stream_partial(
                                agent_p.clone(),
                                tool_use_id,
                                uri.clone(),
                                prefix,
                            );
                        }
                    })
                    .with_abandon_hook(move |tool_use_id, name| {
                        if name == EXECUTE {
                            abandon.stream_cancel(agent_a.clone(), Some(tool_use_id.to_string()));
                        }
                    });
            let mut api = ApiConfig::new(Arc::new(exec));
            // The panel sends the state of its own document as context.
            api.prefetch_app_state = false;
            if let Some(u) = &env.api_base_url {
                api.base_url.clone_from(u);
            }
            api.api_key.clone_from(&env.api_key);
            EngineConfig::Api(api)
        }
    };
    Ok(SessionConfig {
        engine,
        system_prompt: system_prompt(env.skills_dir.as_deref()),
        model: settings.model(),
        effort: settings.effort(),
        resume: match settings.engine {
            EngineKind::ClaudeCode => env.resume.clone(),
            EngineKind::AnthropicApi => None,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> EngineEnv {
        EngineEnv {
            backend: GuiBackend::new().0,
            skills_dir: None,
            cli_path: Some(PathBuf::from("C:/x/openpen-cli.exe")),
            socket: Some("desktop".into()),
            doc_uri: Some("C:/d/a.pen".into()),
            agent_id: "chat-1".into(),
            resume: Some("sess".into()),
            api_base_url: None,
            api_key: None,
        }
    }

    #[test]
    fn claude_code_config() {
        let s = ChatSettings {
            cli_model: "haiku".into(),
            ..ChatSettings::default()
        };
        let c = session_config(&s, &env()).unwrap();
        assert_eq!(c.model.as_deref(), Some("haiku"));
        assert_eq!(c.resume.as_deref(), Some("sess"));
        assert!(c.system_prompt.contains("<final_message>"));
        match c.engine {
            EngineConfig::ClaudeCli(cli) => {
                assert_eq!(cli.mcp_socket, None, "default socket");
                assert_eq!(cli.agent_name, CHAT_LABEL);
                assert_eq!(cli.extra_args, vec!["--setting-sources", ""]);
            }
            EngineConfig::Api(_) => panic!("wrong engine"),
        }
        let mut e = env();
        e.socket = Some("dev".into());
        match session_config(&s, &e).unwrap().engine {
            EngineConfig::ClaudeCli(cli) => assert_eq!(cli.mcp_socket.as_deref(), Some("dev")),
            EngineConfig::Api(_) => panic!(),
        }
        e.socket = None;
        assert!(session_config(&s, &e).err().unwrap().contains("MCP server"));
        e.cli_path = None;
        assert!(
            session_config(&s, &e)
                .err()
                .unwrap()
                .contains("openpen-cli")
        );
    }

    #[test]
    fn api_config() {
        let s = ChatSettings {
            engine: EngineKind::AnthropicApi,
            ..ChatSettings::default()
        };
        let c = session_config(&s, &env()).unwrap();
        assert_eq!(c.model.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(c.resume, None, "the API engine keeps no server history");
        match c.engine {
            EngineConfig::Api(api) => assert!(!api.prefetch_app_state),
            EngineConfig::ClaudeCli(_) => panic!("wrong engine"),
        }
    }
}
