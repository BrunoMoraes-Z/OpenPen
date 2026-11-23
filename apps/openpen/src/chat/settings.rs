//! Engine settings of the chat (engine, model, effort, Claude Code
//! isolation), stored as `chat-settings.json` in the app data dir. The API
//! key never goes here: it lives in the OS credential store
//! (`pen_agent::key`). Also resolves the `openpen-cli` executable that the
//! Claude Code engine starts as its MCP server.

use std::path::{Path, PathBuf};

use pen_agent::Effort;
use serde::{Deserialize, Serialize};

pub const FILE_NAME: &str = "chat-settings.json";

/// Which engine runs the chat.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineKind {
    /// The installed Claude Code CLI (default).
    #[default]
    ClaudeCode,
    /// The Anthropic Messages API with the user's key.
    AnthropicApi,
}

impl EngineKind {
    pub fn label(self) -> &'static str {
        match self {
            EngineKind::ClaudeCode => "Claude Code",
            EngineKind::AnthropicApi => "Anthropic API",
        }
    }
}

/// Claude Code model choices (aliases; `default` = the CLI's own default).
pub const CLI_MODELS: [(&str, &str); 4] = [
    ("default", "Default"),
    ("sonnet", "Sonnet"),
    ("opus", "Opus"),
    ("haiku", "Haiku"),
];

/// Anthropic API model presets (the first is the default).
pub const API_MODELS: [(&str, &str); 3] = [
    ("claude-opus-5-5", "Opus 5.5"),
    ("claude-sonnet-5-5", "Sonnet 5.5"),
    ("claude-haiku-5-5", "Haiku 5.5"),
];

pub const EFFORTS: [(Effort, &str); 5] = [
    (Effort::Low, "Low"),
    (Effort::Medium, "Medium"),
    (Effort::High, "High"),
    (Effort::XHigh, "Extra high"),
    (Effort::Max, "Max"),
];

/// Persisted chat settings. Unknown or missing fields fall back to the
/// defaults, so an old or hand-edited file never breaks the chat.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChatSettings {
    pub engine: EngineKind,
    /// Claude Code model alias (`default`, `sonnet`, `opus`, `haiku`).
    pub cli_model: String,
    /// API model id (a preset or a custom id).
    pub api_model: String,
    /// `low`, `medium`, `high`, `xhigh` or `max`.
    pub effort: String,
    /// Claude Code: load the user's own settings, hooks and plugins. Off by
    /// default: the chat runs with `--setting-sources ""`.
    pub use_user_settings: bool,
    /// Allow WebSearch / WebFetch (Claude Code only).
    pub allow_web: bool,
}

impl Default for ChatSettings {
    fn default() -> Self {
        ChatSettings {
            engine: EngineKind::ClaudeCode,
            cli_model: "default".into(),
            api_model: pen_agent::api::DEFAULT_MODEL.into(),
            effort: "low".into(),
            use_user_settings: false,
            allow_web: true,
        }
    }
}

impl ChatSettings {
    pub fn effort(&self) -> Effort {
        parse_effort(&self.effort)
    }

    pub fn set_effort(&mut self, e: Effort) {
        self.effort = e.as_str().into();
    }

    /// The model to pass to the session (`None`: the engine's default).
    pub fn model(&self) -> Option<String> {
        match self.engine {
            EngineKind::ClaudeCode => {
                let m = self.cli_model.trim();
                (!m.is_empty() && m != "default").then(|| m.to_string())
            }
            EngineKind::AnthropicApi => {
                let m = self.api_model.trim();
                Some(if m.is_empty() {
                    pen_agent::api::DEFAULT_MODEL.to_string()
                } else {
                    m.to_string()
                })
            }
        }
    }

    /// Short label of the model for the header (`Haiku`, `Opus 5.5`, or the
    /// custom id).
    pub fn model_label(&self) -> String {
        match self.engine {
            EngineKind::ClaudeCode => CLI_MODELS
                .iter()
                .find(|(id, _)| *id == self.cli_model.trim())
                .map(|(_, l)| l.to_string())
                .unwrap_or_else(|| self.cli_model.clone()),
            EngineKind::AnthropicApi => API_MODELS
                .iter()
                .find(|(id, _)| *id == self.api_model.trim())
                .map(|(_, l)| l.to_string())
                .unwrap_or_else(|| self.api_model.trim().to_string()),
        }
    }

    /// Extra `claude` arguments: isolation from the user's own Claude Code
    /// settings (hooks, plugins, permissions) unless they opted in.
    pub fn cli_extra_args(&self) -> Vec<String> {
        if self.use_user_settings {
            Vec::new()
        } else {
            vec!["--setting-sources".into(), String::new()]
        }
    }

    pub fn load(dir: &Path) -> Self {
        std::fs::read_to_string(dir.join(FILE_NAME))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        crate::save::write_atomic(&dir.join(FILE_NAME), self.to_json().as_bytes())
    }
}

pub fn parse_effort(s: &str) -> Effort {
    match s.trim() {
        "medium" => Effort::Medium,
        "high" => Effort::High,
        "xhigh" => Effort::XHigh,
        "max" => Effort::Max,
        _ => Effort::Low,
    }
}

pub fn effort_label(e: Effort) -> &'static str {
    EFFORTS
        .iter()
        .find(|(x, _)| *x == e)
        .map(|(_, l)| *l)
        .unwrap_or("Low")
}

fn cli_file_name() -> &'static str {
    if cfg!(windows) {
        "openpen-cli.exe"
    } else {
        "openpen-cli"
    }
}

/// Finds `openpen-cli`: next to the running executable, else in the cargo
/// target dir (`$CARGO_TARGET_DIR`, or a `target` folder above the
/// executable or the source tree; release before debug), else on `PATH`.
pub fn resolve_openpen_cli(
    exe: Option<&Path>,
    target_dirs: &[PathBuf],
    path_var: Option<&std::ffi::OsStr>,
    is_file: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let name = cli_file_name();
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(dir) = exe.and_then(Path::parent) {
        candidates.push(dir.join(name));
    }
    let mut targets: Vec<PathBuf> = target_dirs.to_vec();
    if let Some(exe) = exe
        && let Some(t) = exe
            .ancestors()
            .skip(1)
            .find(|d| d.file_name().is_some_and(|n| n == "target"))
    {
        targets.insert(0, t.to_path_buf());
    }
    for t in targets {
        candidates.push(t.join("release").join(name));
        candidates.push(t.join("debug").join(name));
    }
    if let Some(p) = path_var {
        for d in std::env::split_paths(p) {
            candidates.push(d.join(name));
        }
    }
    candidates.into_iter().find(|c| is_file(c))
}

/// [`resolve_openpen_cli`] for this process.
pub fn openpen_cli() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok();
    let mut targets = Vec::new();
    if let Some(t) = std::env::var_os("CARGO_TARGET_DIR").filter(|t| !t.is_empty()) {
        targets.push(PathBuf::from(t));
    }
    // The workspace's target dir (dev builds run from a copied exe).
    targets.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"));
    resolve_openpen_cli(
        exe.as_deref(),
        &targets,
        std::env::var_os("PATH").as_deref(),
        Path::is_file,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn settings_round_trip_and_defaults() {
        let dir =
            std::env::temp_dir().join(format!("openpen-chat-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // Missing file: defaults.
        let s = ChatSettings::load(&dir);
        assert_eq!(s, ChatSettings::default());
        assert_eq!(s.model(), None);
        assert_eq!(s.effort(), Effort::Low);
        assert_eq!(s.cli_extra_args(), vec!["--setting-sources", ""]);
        let mut s = ChatSettings {
            engine: EngineKind::AnthropicApi,
            api_model: "claude-sonnet-5-5".into(),
            use_user_settings: true,
            ..ChatSettings::default()
        };
        s.set_effort(Effort::XHigh);
        s.save(&dir).unwrap();
        let back = ChatSettings::load(&dir);
        assert_eq!(back, s);
        assert_eq!(back.model().as_deref(), Some("claude-sonnet-5-5"));
        assert_eq!(back.model_label(), "Sonnet 5.5");
        assert_eq!(back.effort(), Effort::XHigh);
        assert!(back.cli_extra_args().is_empty());
        // No key ever ends up in the file.
        let text = std::fs::read_to_string(dir.join(FILE_NAME)).unwrap();
        assert!(!text.to_ascii_lowercase().contains("key"));
        // Partial / unknown fields and broken files.
        std::fs::write(dir.join(FILE_NAME), r#"{"engine":"anthropic_api","x":1}"#).unwrap();
        let p = ChatSettings::load(&dir);
        assert_eq!(p.engine, EngineKind::AnthropicApi);
        assert_eq!(p.api_model, "claude-opus-5-5");
        assert_eq!(p.model().as_deref(), Some("claude-opus-5-5"));
        std::fs::write(dir.join(FILE_NAME), "{not json").unwrap();
        assert_eq!(ChatSettings::load(&dir), ChatSettings::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn model_choices() {
        let mut s = ChatSettings {
            cli_model: "haiku".into(),
            ..ChatSettings::default()
        };
        assert_eq!(s.model().as_deref(), Some("haiku"));
        assert_eq!(s.model_label(), "Haiku");
        s.engine = EngineKind::AnthropicApi;
        s.api_model = " my-custom-model ".into();
        assert_eq!(s.model().as_deref(), Some("my-custom-model"));
        assert_eq!(s.model_label(), "my-custom-model");
        s.api_model.clear();
        assert_eq!(s.model().as_deref(), Some("claude-opus-5-5"));
        assert_eq!(parse_effort("nope"), Effort::Low);
        assert_eq!(effort_label(Effort::Max), "Max");
    }

    #[test]
    fn cli_resolution_order() {
        let name = cli_file_name();
        let root = PathBuf::from(if cfg!(windows) { "C:\\w" } else { "/w" });
        let exe = root.join("target").join("demo").join("openpen-demo.exe");
        let files = |fs: &[PathBuf]| fs.iter().cloned().collect::<HashSet<_>>();
        let resolve = |fs: &HashSet<PathBuf>, path: Option<&str>| {
            resolve_openpen_cli(
                Some(&exe),
                &[root.join("other-target")],
                path.map(std::ffi::OsStr::new),
                |p| fs.contains(p),
            )
        };
        let next_to = exe.parent().unwrap().join(name);
        let release = root.join("target").join("release").join(name);
        let debug = root.join("target").join("debug").join(name);
        let other = root.join("other-target").join("debug").join(name);
        let on_path = root.join("bin").join(name);
        let path_var = std::env::join_paths([root.join("bin")]).unwrap();
        let path_var = path_var.to_str().unwrap();
        // Next to the exe wins.
        let all = files(&[
            next_to.clone(),
            release.clone(),
            debug.clone(),
            on_path.clone(),
        ]);
        assert_eq!(resolve(&all, Some(path_var)), Some(next_to));
        // Then the target dir above the exe, release first.
        let fs = files(&[release.clone(), debug.clone(), on_path.clone()]);
        assert_eq!(resolve(&fs, Some(path_var)), Some(release));
        let fs = files(&[debug.clone(), other.clone()]);
        assert_eq!(resolve(&fs, None), Some(debug));
        // Then the extra target dirs, then PATH.
        assert_eq!(
            resolve(&files(std::slice::from_ref(&other)), None),
            Some(other)
        );
        assert_eq!(
            resolve(&files(std::slice::from_ref(&on_path)), Some(path_var)),
            Some(on_path)
        );
        assert_eq!(resolve(&files(&[]), Some(path_var)), None);
    }
}
