//! Tools of the API engine: their definitions (the same JSON Schemas the MCP
//! server advertises), the [`ToolExecutor`] seam the app implements, a ready
//! executor over a `pen_mcp::ToolBackend`, and input validation.

use std::sync::Arc;

use pen_mcp::tools::{
    self as mcp_tools, ExecuteArgs, GetAppStateArgs, ImportWebsiteArgs, ReadSkillArgs, SaveArgs,
    StartTaskArgs,
};
use pen_mcp::{BoxFuture, CallCtx, SkillStore, ToolBackend, ToolOutput};
use serde_json::{Map, Value, json};

/// Prefix Claude Code puts on the tools of our MCP server.
pub const MCP_PREFIX: &str = "mcp__openpen__";

/// Tool name without the MCP prefix (`mcp__openpen__execute` -> `execute`).
pub fn normalize_tool_name(name: &str) -> &str {
    name.strip_prefix(MCP_PREFIX).unwrap_or(name)
}

/// A client tool as sent to the Messages API.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    /// JSON Schema of the input (an object schema).
    pub input_schema: Value,
}

impl ToolDef {
    /// The API form, with eager input streaming on (the input arrives
    /// unbuffered and unvalidated; see [`validate_input`]).
    pub fn to_api_json(&self) -> Value {
        json!({
            "name": self.name,
            "description": self.description,
            "input_schema": self.input_schema,
            "eager_input_streaming": true,
        })
    }
}

/// The OpenPen tools (`start_task`, `read_skill`, `get_app_state`,
/// `execute`, `save` with `include_save`, and `import_website`), with the schemas of
/// `pen-mcp`. `get_style` is left out: it only returns a notice.
pub fn pen_tool_defs(include_save: bool) -> Vec<ToolDef> {
    mcp_tools::tool_list(include_save)
        .into_iter()
        .filter(|t| t.name != mcp_tools::GET_STYLE)
        .map(|t| {
            let mut schema: Map<String, Value> = (*t.input_schema).clone();
            // Metadata keys the API does not need.
            schema.remove("$schema");
            schema.remove("title");
            ToolDef {
                name: t.name.to_string(),
                description: t.description.map(|d| d.to_string()).unwrap_or_default(),
                input_schema: Value::Object(schema),
            }
        })
        .collect()
}

/// Runs the tools of the API engine. Implemented by the app (in-process),
/// or use [`BackendExecutor`].
pub trait ToolExecutor: Send + Sync + 'static {
    /// Runs one tool call. `input` already passed [`validate_input`].
    fn call(&self, name: &str, input: Value, tool_use_id: &str) -> BoxFuture<'static, ToolOutput>;

    /// The input of a tool call grew; `partial_json` is all of it so far.
    /// Drives streaming `execute` (see [`crate::execute_input_prefix`]).
    fn on_partial_input(&self, tool_use_id: &str, name: &str, partial_json: &str) {
        let _ = (tool_use_id, name, partial_json);
    }

    /// A call that got `on_partial_input` will not run (the stream was cut,
    /// the user stopped the turn, the input was invalid, ...). A streaming
    /// `execute` should be rolled back (`ScriptEngine::cancel_stream`).
    fn on_input_abandoned(&self, tool_use_id: &str, name: &str) {
        let _ = (tool_use_id, name);
    }
}

type PartialHook = Arc<dyn Fn(&str, &str, &str) + Send + Sync>;
type AbandonHook = Arc<dyn Fn(&str, &str) + Send + Sync>;

/// A [`ToolExecutor`] over a `pen_mcp::ToolBackend`, answering the skill
/// tools itself like the MCP server does.
#[derive(Clone)]
pub struct BackendExecutor {
    backend: Arc<dyn ToolBackend>,
    skills: Option<SkillStore>,
    ctx: CallCtx,
    enable_save: bool,
    on_partial: Option<PartialHook>,
    on_abandon: Option<AbandonHook>,
}

impl BackendExecutor {
    /// `ctx` identifies the chat to the backend (e.g. `agent_name`
    /// `"OpenPen Chat"`, `client_id` per session).
    pub fn new(
        backend: Arc<dyn ToolBackend>,
        skills_dir: Option<std::path::PathBuf>,
        ctx: CallCtx,
    ) -> Self {
        Self {
            backend,
            skills: skills_dir.map(SkillStore::new),
            ctx,
            enable_save: true,
            on_partial: None,
            on_abandon: None,
        }
    }

    pub fn with_save(mut self, enable: bool) -> Self {
        self.enable_save = enable;
        self
    }

    /// Hooks for streaming execute: `(tool_use_id, name, partial_json)`.
    pub fn with_partial_hook(
        mut self,
        f: impl Fn(&str, &str, &str) + Send + Sync + 'static,
    ) -> Self {
        self.on_partial = Some(Arc::new(f));
        self
    }

    /// Called with `(tool_use_id, name)` when a streamed call will not run.
    pub fn with_abandon_hook(mut self, f: impl Fn(&str, &str) + Send + Sync + 'static) -> Self {
        self.on_abandon = Some(Arc::new(f));
        self
    }

    fn skill_store(&self) -> Result<&SkillStore, ToolOutput> {
        self.skills.as_ref().ok_or_else(|| {
            ToolOutput::error(
                "OpenPen skills are not installed (skills folder not found). \
                 Set OPENPEN_SKILLS_DIR to the `skills/openpen` folder.",
            )
        })
    }
}

fn parse<T: serde::de::DeserializeOwned>(input: Value) -> Result<T, ToolOutput> {
    serde_json::from_value(input).map_err(|e| ToolOutput::error(format!("invalid arguments: {e}")))
}

fn done(out: ToolOutput) -> BoxFuture<'static, ToolOutput> {
    Box::pin(async move { out })
}

fn backend_call(fut: BoxFuture<'static, pen_mcp::ToolResult>) -> BoxFuture<'static, ToolOutput> {
    Box::pin(async move {
        match fut.await {
            Ok(out) => out,
            Err(e) => ToolOutput::error(e.to_string()),
        }
    })
}

impl ToolExecutor for BackendExecutor {
    fn call(&self, name: &str, input: Value, tool_use_id: &str) -> BoxFuture<'static, ToolOutput> {
        let mut ctx = self.ctx.clone();
        // Same tag Claude Code puts in the MCP `_meta`: lets the backend
        // match the final `execute` with its streamed input.
        ctx.tool_use_id = Some(tool_use_id.to_string());
        let r: Result<BoxFuture<'static, ToolOutput>, ToolOutput> = (|| match name {
            mcp_tools::GET_APP_STATE => {
                let a: GetAppStateArgs = parse(input)?;
                Ok(backend_call(self.backend.get_app_state(ctx, a.uri)))
            }
            mcp_tools::EXECUTE => {
                let a: ExecuteArgs = parse(input)?;
                a.validate().map_err(ToolOutput::error)?;
                Ok(backend_call(self.backend.execute(ctx, a)))
            }
            mcp_tools::SAVE if self.enable_save => {
                let a: SaveArgs = parse(input)?;
                Ok(backend_call(self.backend.save(ctx, a.uri)))
            }
            mcp_tools::IMPORT_WEBSITE => {
                let a: ImportWebsiteArgs = parse(input)?;
                let url = a.validate().map_err(ToolOutput::error)?;
                Ok(backend_call(self.backend.import_website(ctx, a, url)))
            }
            mcp_tools::READ_SKILL => {
                let a: ReadSkillArgs = parse(input)?;
                let s = self.skill_store()?;
                Ok(done(match s.read(a.path.as_deref()) {
                    Ok(t) => ToolOutput::text(t),
                    Err(e) => ToolOutput::error(e.to_string()),
                }))
            }
            mcp_tools::START_TASK => {
                let a: StartTaskArgs = parse(input)?;
                let s = self.skill_store()?;
                Ok(done(match s.bundle(&a.skills, a.title.as_deref()) {
                    Ok(t) => ToolOutput::text(t),
                    Err(e) => ToolOutput::error(e.to_string()),
                }))
            }
            other => Err(ToolOutput::error(format!("unknown tool `{other}`"))),
        })();
        r.unwrap_or_else(done)
    }

    fn on_partial_input(&self, tool_use_id: &str, name: &str, partial_json: &str) {
        if let Some(f) = &self.on_partial {
            f(tool_use_id, name, partial_json);
        }
    }

    fn on_input_abandoned(&self, tool_use_id: &str, name: &str) {
        if let Some(f) = &self.on_abandon {
            f(tool_use_id, name);
        }
    }
}

/// Checks `input` against a tool's JSON Schema: `type` (also as a list),
/// `required`, `properties`, `items`, `enum`, `anyOf`/`oneOf` and local
/// `$ref`s (`#/$defs/..`, `#/definitions/..`). Other keywords are ignored.
pub fn validate_input(schema: &Value, input: &Value) -> Result<(), String> {
    check(schema, schema, input, "input")
}

fn resolve<'a>(root: &'a Value, schema: &'a Value) -> &'a Value {
    let mut s = schema;
    for _ in 0..16 {
        let Some(r) = s.get("$ref").and_then(Value::as_str) else {
            break;
        };
        let Some(ptr) = r.strip_prefix('#') else {
            break;
        };
        match root.pointer(ptr) {
            Some(t) => s = t,
            None => break,
        }
    }
    s
}

fn type_matches(t: &str, v: &Value) -> bool {
    match t {
        "object" => v.is_object(),
        "array" => v.is_array(),
        "string" => v.is_string(),
        "boolean" => v.is_boolean(),
        "null" => v.is_null(),
        "number" => v.is_number(),
        "integer" => v.as_i64().is_some() || v.as_u64().is_some(),
        _ => true,
    }
}

fn check(root: &Value, schema: &Value, v: &Value, path: &str) -> Result<(), String> {
    let schema = resolve(root, schema);
    if let Some(t) = schema.get("type") {
        let ok = match t {
            Value::String(t) => type_matches(t, v),
            Value::Array(ts) => ts
                .iter()
                .filter_map(Value::as_str)
                .any(|t| type_matches(t, v)),
            _ => true,
        };
        if !ok {
            return Err(format!("`{path}` must be of type {t}"));
        }
    }
    if let Some(Value::Array(options)) = schema.get("enum")
        && !options.contains(v)
    {
        return Err(format!(
            "`{path}` must be one of {}",
            Value::Array(options.clone())
        ));
    }
    for key in ["anyOf", "oneOf"] {
        if let Some(Value::Array(branches)) = schema.get(key)
            && !branches.iter().any(|b| check(root, b, v, path).is_ok())
        {
            return Err(format!("`{path}` matches none of the allowed shapes"));
        }
    }
    if let Value::Object(obj) = v {
        if let Some(Value::Array(req)) = schema.get("required") {
            for k in req.iter().filter_map(Value::as_str) {
                if !obj.contains_key(k) {
                    return Err(format!("missing required field `{k}` in `{path}`"));
                }
            }
        }
        if let Some(Value::Object(props)) = schema.get("properties") {
            for (k, sub) in props {
                if let Some(val) = obj.get(k) {
                    check(root, sub, val, &format!("{path}.{k}"))?;
                }
            }
        }
    }
    if let (Value::Array(items), Some(item_schema)) = (v, schema.get("items")) {
        for (i, it) in items.iter().enumerate() {
            check(root, item_schema, it, &format!("{path}[{i}]"))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defs_from_pen_mcp() {
        let defs = pen_tool_defs(true);
        let names: Vec<_> = defs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "start_task",
                "read_skill",
                "get_app_state",
                "execute",
                "save",
                "import_website"
            ]
        );
        assert!(!pen_tool_defs(false).iter().any(|d| d.name == "save"));
        for d in &defs {
            assert_eq!(d.input_schema["type"], "object");
            assert!(d.input_schema.get("$schema").is_none());
            let j = d.to_api_json();
            assert_eq!(j["eager_input_streaming"], true);
        }
        // Deterministic serialization (part of the cached prefix).
        let a = serde_json::to_string(&defs.iter().map(ToolDef::to_api_json).collect::<Vec<_>>())
            .unwrap();
        let b = serde_json::to_string(
            &pen_tool_defs(true)
                .iter()
                .map(ToolDef::to_api_json)
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn validates_execute_inputs() {
        let exec = pen_tool_defs(true)
            .into_iter()
            .find(|d| d.name == "execute")
            .unwrap();
        let s = &exec.input_schema;
        assert!(validate_input(s, &json!({"description": "d", "input": "Print(1)"})).is_ok());
        assert!(
            validate_input(
                s,
                &json!({"description": "d", "editId": "e1",
            "edits": [{"find": "a", "replace": "b"}]})
            )
            .is_ok()
        );
        assert!(
            validate_input(s, &json!({"input": "x"}))
                .unwrap_err()
                .contains("description")
        );
        assert!(validate_input(s, &json!({"description": 3})).is_err());
        assert!(validate_input(s, &json!({"description": "d", "edits": [{"find": "a"}]})).is_err());
        assert!(validate_input(s, &json!({"description": "d", "uri": null, "input": "x"})).is_ok());
        assert!(validate_input(s, &json!("str")).is_err());
        let start = pen_tool_defs(true)
            .into_iter()
            .find(|d| d.name == "start_task")
            .unwrap();
        assert!(validate_input(&start.input_schema, &json!({"skills": ["a.md"]})).is_ok());
        assert!(validate_input(&start.input_schema, &json!({"skills": [1]})).is_err());
    }

    #[test]
    fn names() {
        assert_eq!(normalize_tool_name("mcp__openpen__execute"), "execute");
        assert_eq!(normalize_tool_name("WebSearch"), "WebSearch");
    }

    struct Fake;
    impl ToolBackend for Fake {
        fn get_app_state(
            &self,
            ctx: CallCtx,
            uri: Option<String>,
        ) -> BoxFuture<'static, pen_mcp::ToolResult> {
            Box::pin(async move {
                Ok(ToolOutput::text(format!(
                    "{} {:?} {}",
                    ctx.agent_name,
                    uri,
                    ctx.tool_use_id.unwrap_or_default()
                )))
            })
        }
        fn execute(
            &self,
            _: CallCtx,
            args: ExecuteArgs,
        ) -> BoxFuture<'static, pen_mcp::ToolResult> {
            Box::pin(async move {
                if args.input.as_deref() == Some("fail") {
                    Err(pen_mcp::ToolError::Failed("boom".into()))
                } else {
                    Ok(ToolOutput::text("ran").with_image(vec![1, 2]))
                }
            })
        }
        fn save(&self, _: CallCtx, _: Option<String>) -> BoxFuture<'static, pen_mcp::ToolResult> {
            Box::pin(async { Ok(ToolOutput::text("saved")) })
        }
    }

    #[test]
    fn backend_executor_dispatch() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/openpen");
        let ctx = CallCtx {
            client_id: "chat-1".into(),
            agent_name: "OpenPen Chat".into(),
            ..CallCtx::default()
        };
        let ex = BackendExecutor::new(Arc::new(Fake), Some(dir.clone()), ctx);
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(async {
            let o = ex.call("get_app_state", json!({}), "t1").await;
            assert_eq!(o.text, "OpenPen Chat None t1");
            let o = ex
                .call("execute", json!({"description": "d", "input": "x"}), "t2")
                .await;
            assert!(!o.is_error);
            assert_eq!(o.images.len(), 1);
            let o = ex
                .call(
                    "execute",
                    json!({"description": "d", "input": "fail"}),
                    "t3",
                )
                .await;
            assert!(o.is_error && o.text == "boom");
            let o = ex.call("execute", json!({"description": "d"}), "t4").await;
            assert!(o.is_error);
            let o = ex.call("nope", json!({}), "t5").await;
            assert!(o.is_error);
            if dir.exists() {
                let o = ex.call("read_skill", json!({}), "t6").await;
                assert!(!o.is_error && o.text.contains("execute.md"));
                let o = ex
                    .call("start_task", json!({"skills": ["SKILL.md"]}), "t7")
                    .await;
                assert!(!o.is_error);
            }
            let ex2 = ex.clone().with_save(false);
            assert!(ex2.call("save", json!({}), "t8").await.is_error);
            // `Fake` keeps the trait's default: a readable error, and bad
            // addresses are refused before reaching the backend.
            let o = ex
                .call("import_website", json!({"url": "example.com"}), "t9")
                .await;
            assert!(o.is_error && o.text.contains("cannot import"), "{}", o.text);
            let o = ex
                .call("import_website", json!({"url": "ftp://x.y"}), "t10")
                .await;
            assert!(o.is_error && o.text.contains("http"), "{}", o.text);
        });
    }
}
