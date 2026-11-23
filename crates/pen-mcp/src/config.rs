//! Snippets for registering OpenPen in MCP clients.
//!
//! Installation is opt-in: these functions only build text for the user to
//! copy; nothing here writes any configuration file.

use std::path::Path;

use serde_json::{Value, json};

/// Arguments that make `openpen-cli` act as the stdio shim.
pub fn shim_args(socket: Option<&str>, agent: Option<&str>) -> Vec<String> {
    let mut args = vec!["mcp".to_string()];
    if let Some(s) = socket {
        args.extend(["--socket".to_string(), s.to_string()]);
    }
    if let Some(a) = agent {
        args.extend(["--agent".to_string(), a.to_string()]);
    }
    args
}

/// The `mcpServers` entry for Claude Code (`~/.claude.json` or a project
/// `.mcp.json`) that launches `openpen-cli mcp`.
pub fn claude_code_config(cli_path: &Path, socket: Option<&str>) -> Value {
    json!({
        "mcpServers": {
            "openpen": {
                "type": "stdio",
                "command": cli_path.display().to_string(),
                "args": shim_args(socket, None),
            }
        }
    })
}

/// Same as [`claude_code_config`], pretty-printed.
pub fn claude_code_config_string(cli_path: &Path, socket: Option<&str>) -> String {
    serde_json::to_string_pretty(&claude_code_config(cli_path, socket)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippet_shape() {
        let v = claude_code_config(Path::new("/opt/openpen/openpen-cli"), None);
        assert_eq!(
            v["mcpServers"]["openpen"]["command"],
            "/opt/openpen/openpen-cli"
        );
        assert_eq!(v["mcpServers"]["openpen"]["args"], json!(["mcp"]));
        let v = claude_code_config(Path::new("x"), Some("dev"));
        assert_eq!(
            v["mcpServers"]["openpen"]["args"],
            json!(["mcp", "--socket", "dev"])
        );
        assert!(claude_code_config_string(Path::new("x"), None).contains("\"mcpServers\""));
    }
}
