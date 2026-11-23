//! The chat panel (left sidebar, Chat tab): header with the conversation
//! title, the engine settings and "New"; the message list (markdown answers,
//! collapsed thinking, compact tool rows); the document-locked strip and the
//! composer. One panel per document tab; see docs/chat.md.
//!
//! Agent events arrive on the session's own thread and are drained by a task
//! on the GPUI foreground, which feeds the [`ChatModel`] reducer and carries
//! out its effects (stream partials, cancellations, persistence).

use std::collections::HashSet;
use std::path::PathBuf;

use futures::StreamExt as _;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::highlighter::HighlightTheme;
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::text::{TextView, TextViewStyle};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, Sizable, WindowExt};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use pen_agent::{AgentEvent, Session, UserMessage, start_session};
use pen_mcp::{CallCtx, ToolBackend as _};

use super::engine::{CHAT_LABEL, EngineEnv, session_config};
use super::model::{ChatModel, EXECUTE, Effect, ErrorKind, Item, ToolCall, ToolState};
use super::settings::{
    API_MODELS, CLI_MODELS, ChatSettings, EFFORTS, EngineKind, effort_label, openpen_cli,
};
use super::store::{self, StoredConversation};
use crate::agent::backend::GuiBackend;
use crate::agent::hub::AgentHub;
use crate::editor_state::EditorState;
use crate::theme::{Palette, PaletteExt, metrics};
use crate::workspace::log;

/// The settings of every chat panel (GPUI global, saved on change).
pub struct ChatSettingsGlobal(pub ChatSettings);

impl Global for ChatSettingsGlobal {}

fn settings(cx: &mut App) -> ChatSettings {
    if cx.try_global::<ChatSettingsGlobal>().is_none() {
        let s = crate::recent::data_dir()
            .map(|d| ChatSettings::load(&d))
            .unwrap_or_default();
        cx.set_global(ChatSettingsGlobal(s));
    }
    cx.global::<ChatSettingsGlobal>().0.clone()
}

fn save_settings(s: ChatSettings, cx: &mut App) {
    cx.set_global(ChatSettingsGlobal(s.clone()));
    if let Some(dir) = crate::recent::data_dir() {
        cx.background_spawn(async move {
            if let Err(e) = s.save(&dir) {
                log(&format!("chat: could not save the settings: {e}"));
            }
        })
        .detach();
    }
}

/// Example prompts of the empty state.
const EXAMPLES: [&str; 3] = [
    "Design a login card with email, password and a sign-in button",
    "Design a playful ordering screen for a matcha café",
    "Design a pricing section with three plans",
];

/// A running session and what its effects need.
struct Live {
    session: Option<Session>,
    backend: GuiBackend,
    engine: EngineKind,
    /// Agent id of the streams and the canvas fx: the MCP agent name
    /// (Claude Code engine) or the in-process client id (API engine).
    stream_id: String,
    /// The first message carries the editor state as context.
    context_sent: bool,
    _drain: Task<()>,
}

impl Drop for Live {
    fn drop(&mut self) {
        self.backend.stream_cancel(self.stream_id.clone(), None);
        if self.engine == EngineKind::AnthropicApi {
            // In-process agent: no MCP disconnect will clean it up.
            self.backend.agent_disconnected(&CallCtx {
                client_id: self.stream_id.clone(),
                agent_name: CHAT_LABEL.into(),
                ..Default::default()
            });
        }
        // Dropping a session waits for its worker (kills Claude Code): never
        // on the UI thread.
        if let Some(s) = self.session.take() {
            let _ = std::thread::Builder::new()
                .name("pen-agent-drop".into())
                .spawn(move || drop(s));
        }
    }
}

pub struct ChatPanel {
    state: Entity<EditorState>,
    model: ChatModel,
    conv_id: String,
    created_ms: u64,
    live: Option<Live>,
    /// Settings changed while a turn ran: restart the session after it.
    restart_needed: bool,
    input: Entity<TextareaState>,
    key_input: Entity<InputState>,
    custom_model: Entity<InputState>,
    scroll: ScrollHandle,
    follow: bool,
    last_rev: u64,
    /// Expanded tool rows (tool id) and thinking blocks (`think-<index>`).
    expanded: HashSet<String>,
    settings_open: bool,
    /// Masked stored API key (read when the settings open).
    key_mask: Option<String>,
    _subs: Vec<Subscription>,
}

impl ChatPanel {
    pub fn new(state: Entity<EditorState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 10)
                .submit_on_enter(true)
                .placeholder("Ask for a design or a change…")
        });
        let key_input = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("sk-ant-…")
        });
        let s = settings(cx);
        let custom = if API_MODELS.iter().any(|(id, _)| *id == s.api_model) {
            String::new()
        } else {
            s.api_model.clone()
        };
        let custom_model = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Custom model id")
                .default_value(custom)
        });
        let subs = vec![
            cx.subscribe_in(
                &input,
                window,
                |this, input, ev: &InputEvent, window, cx| {
                    if let InputEvent::PressEnter { shift: false, .. } = ev {
                        let text = input.read(cx).value().to_string();
                        if !text.trim().is_empty() {
                            input.update(cx, |s, cx| s.set_value("", window, cx));
                            this.send(text, cx);
                        }
                    }
                },
            ),
            cx.subscribe_in(
                &key_input,
                window,
                |this, _, ev: &InputEvent, window, cx| {
                    if let InputEvent::PressEnter { .. } = ev {
                        this.save_key(window, cx);
                    }
                },
            ),
            cx.subscribe_in(&custom_model, window, |this, i, ev: &InputEvent, _, cx| {
                if matches!(ev, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    let v = i.read(cx).value().trim().to_string();
                    if !v.is_empty() {
                        this.update_settings(cx, |s| s.api_model = v);
                    }
                }
            }),
            cx.observe(&state, |_, _, cx| cx.notify()),
        ];
        let mut this = Self {
            state,
            model: ChatModel::new(false),
            conv_id: store::new_id(),
            created_ms: store::now_ms(),
            live: None,
            restart_needed: false,
            input,
            key_input,
            custom_model,
            scroll: ScrollHandle::new(),
            follow: true,
            last_rev: u64::MAX,
            expanded: HashSet::new(),
            settings_open: false,
            key_mask: None,
            _subs: subs,
        };
        this.restore(cx);
        this
    }

    pub fn focus_input(&self, window: &mut Window, cx: &mut App) {
        self.input.update(cx, |s, cx| s.focus(window, cx));
    }

    fn doc_path(&self, cx: &App) -> Option<PathBuf> {
        self.state.read(cx).path.clone()
    }

    fn doc_uri(&self, cx: &App) -> Option<String> {
        self.doc_path(cx).map(|p| p.display().to_string())
    }

    /// Loads the newest conversation of the document (background read).
    fn restore(&mut self, cx: &mut Context<Self>) {
        let (Some(path), Some(data)) = (self.doc_path(cx), crate::recent::data_dir()) else {
            return;
        };
        let dir = store::chats_dir(&data);
        let task = cx.background_spawn(async move { store::latest_for_doc(&dir, &path) });
        cx.spawn(async move |this, cx| {
            let Some(conv) = task.await else { return };
            let _ = this.update(cx, |this, cx| {
                if !this.model.is_empty() || this.live.is_some() {
                    return;
                }
                let session = (conv.engine == EngineKind::ClaudeCode)
                    .then_some(conv.session_id)
                    .flatten();
                this.model = ChatModel::restored(conv.items, session);
                this.conv_id = conv.id;
                this.created_ms = conv.created_ms;
                this.follow = true;
                // Force a scroll to the end on the next render.
                this.last_rev = u64::MAX;
                cx.notify();
            });
        })
        .detach();
    }

    fn persist(&self, cx: &mut Context<Self>) {
        let (Some(path), Some(data)) = (self.doc_path(cx), crate::recent::data_dir()) else {
            return;
        };
        if self.model.is_empty() {
            return;
        }
        let s = settings(cx);
        let mut conv = StoredConversation::new(
            self.conv_id.clone(),
            &crate::workspace::canonical(&path),
            s.engine,
        );
        conv.created_ms = self.created_ms;
        conv.title = self.model.title();
        conv.model = s.model();
        conv.session_id = self.model.session_id.clone();
        conv.items = self.model.items.clone();
        let dir = store::chats_dir(&data);
        cx.background_spawn(async move {
            if let Err(e) = store::save(&dir, &conv) {
                log(&format!("chat: could not save the conversation: {e}"));
            }
        })
        .detach();
    }

    // ------------------------------------------------------------ session

    /// Starts a session with the current settings.
    fn start(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        let s = settings(cx);
        let Some(hub) = cx.try_global::<AgentHub>() else {
            return Err("the agent hub is not running".into());
        };
        let backend = hub.backend.clone();
        let socket = hub.socket().map(str::to_string);
        let agent_id = format!("chat-{}", store::new_id());
        // Resume only a Claude Code session of this conversation.
        let resume = self.model.session_id.clone();
        let env = EngineEnv {
            backend: backend.clone(),
            skills_dir: pen_mcp::find_skills_dir(),
            cli_path: openpen_cli(),
            socket,
            doc_uri: self.doc_uri(cx),
            agent_id: agent_id.clone(),
            resume,
            api_base_url: None,
            api_key: None,
        };
        let cfg = session_config(&s, &env)?;
        let mut session = start_session(cfg).map_err(|e| e.to_string())?;
        let stream_id = session.agent_name().map(str::to_string).unwrap_or(agent_id);
        cx.global_mut::<AgentHub>()
            .register_chat(&stream_id, CHAT_LABEL);
        log(&format!(
            "chat: {} session started ({}, model {})",
            s.engine.label(),
            stream_id,
            s.model().unwrap_or_else(|| "default".into())
        ));
        self.model.stream_partials = s.engine == EngineKind::ClaudeCode;
        let mut rx = session.take_events().ok_or("no event stream")?;
        let drain = cx.spawn(async move |this, cx| {
            while let Some(ev) = rx.next().await {
                let mut batch = vec![ev];
                while let Some(e) = rx.try_recv() {
                    batch.push(e);
                }
                if this.update(cx, |p, cx| p.on_events(batch, cx)).is_err() {
                    break;
                }
            }
        });
        self.live = Some(Live {
            session: Some(session),
            backend,
            engine: s.engine,
            stream_id,
            context_sent: false,
            _drain: drain,
        });
        Ok(())
    }

    fn on_events(&mut self, batch: Vec<AgentEvent>, cx: &mut Context<Self>) {
        let mut persist = false;
        for ev in batch {
            for fx in self.model.apply(ev) {
                match fx {
                    Effect::Persist => persist = true,
                    Effect::StreamPartial {
                        tool_use_id,
                        prefix,
                    } => {
                        if let Some(l) = &self.live {
                            l.backend.stream_partial(
                                l.stream_id.clone(),
                                tool_use_id,
                                self.doc_uri(cx),
                                prefix,
                            );
                        }
                    }
                    Effect::CancelStream { tool_use_id } => {
                        if let Some(l) = &self.live {
                            l.backend
                                .stream_cancel(l.stream_id.clone(), Some(tool_use_id));
                        }
                    }
                }
            }
        }
        if !self.model.running && self.restart_needed {
            self.restart_needed = false;
            self.live = None;
        }
        if persist {
            self.persist(cx);
        }
        cx.notify();
    }

    /// Sends a message (steers the running turn while one runs).
    fn send(&mut self, text: String, cx: &mut Context<Self>) {
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        self.follow = true;
        if self.model.running
            && let Some(s) = self.live.as_ref().and_then(|l| l.session.as_ref())
        {
            if let Err(e) = s.steer(text.clone()) {
                self.model.push_error(e.to_string());
            }
            self.model.push_user(text, true);
            cx.notify();
            return;
        }
        self.model.push_user(text.clone(), false);
        if self.live.is_none()
            && let Err(e) = self.start(cx)
        {
            self.model.push_error(e);
            self.persist(cx);
            cx.notify();
            return;
        }
        self.model.running = true;
        self.persist(cx);
        let uri = self.doc_uri(cx);
        let Some(live) = self.live.as_mut() else {
            return;
        };
        if live.context_sent {
            if let Some(s) = &live.session
                && let Err(e) = s.send(UserMessage::text(text))
            {
                self.model.running = false;
                self.model.push_error(e.to_string());
            }
            cx.notify();
            return;
        }
        live.context_sent = true;
        // First message of the session: prefix the editor state of this
        // panel's document (the same text `get_app_state` returns).
        let ctx = CallCtx {
            client_id: live.stream_id.clone(),
            agent_name: CHAT_LABEL.into(),
            ..Default::default()
        };
        let fut = live.backend.get_app_state(ctx, uri);
        cx.spawn(async move |this, cx| {
            let context = match fut.await {
                Ok(out) if !out.is_error => Some(pen_agent::prompt::app_state_context(&out.text)),
                _ => None,
            };
            let _ = this.update(cx, |this, cx| {
                let msg = UserMessage {
                    text,
                    attachments: Vec::new(),
                    context,
                };
                if let Some(s) = this.live.as_ref().and_then(|l| l.session.as_ref())
                    && let Err(e) = s.send(msg)
                {
                    this.model.running = false;
                    this.model.push_error(e.to_string());
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        if let Some(l) = &self.live {
            if let Some(s) = &l.session {
                s.stop();
            }
            l.backend.stream_cancel(l.stream_id.clone(), None);
        }
        cx.notify();
    }

    /// Stops whatever streams into the document (this chat or another agent).
    fn stop_lock(&mut self, cx: &mut Context<Self>) {
        let lock = self.state.read(cx).doc.stream_lock().map(str::to_string);
        let ours = self
            .live
            .as_ref()
            .is_some_and(|l| Some(&l.stream_id) == lock.as_ref());
        if ours || lock.is_none() {
            self.stop(cx);
        } else if let (Some(agent), Some(hub)) = (lock, cx.try_global::<AgentHub>()) {
            hub.backend.stream_cancel(agent, None);
        }
    }

    fn new_conversation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.live = None;
        self.restart_needed = false;
        self.model = ChatModel::new(false);
        self.conv_id = store::new_id();
        self.created_ms = store::now_ms();
        self.expanded.clear();
        self.follow = true;
        self.focus_input(window, cx);
        cx.notify();
    }

    /// Re-sends the last user message (error banners).
    fn retry(&mut self, cx: &mut Context<Self>) {
        let last = self.model.items.iter().rev().find_map(|i| match i {
            Item::User { text, .. } => Some(text.clone()),
            _ => None,
        });
        // A failed session (process gone, wrong engine) starts afresh.
        self.live = None;
        if let Some(t) = last {
            self.send(t, cx);
        }
    }

    fn update_settings(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut ChatSettings)) {
        let old = settings(cx);
        let mut s = old.clone();
        f(&mut s);
        if s == old {
            return;
        }
        save_settings(s, cx);
        // The next message uses the new settings.
        if self.model.running {
            self.restart_needed = true;
        } else {
            self.live = None;
        }
        cx.notify();
    }

    fn toggle_settings(&mut self, cx: &mut Context<Self>) {
        self.settings_open = !self.settings_open;
        if self.settings_open {
            self.key_mask = pen_agent::key::get_stored_key()
                .ok()
                .flatten()
                .map(|k| pen_agent::key::mask_key(&k));
        }
        cx.notify();
    }

    fn save_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let key = self.key_input.read(cx).value().trim().to_string();
        if key.is_empty() {
            return;
        }
        match pen_agent::key::set_stored_key(&key) {
            Ok(()) => {
                self.key_mask = Some(pen_agent::key::mask_key(&key));
                self.key_input
                    .update(cx, |s, cx| s.set_value("", window, cx));
                self.live = None;
                window.push_notification("API key saved to the system credential store.", cx);
            }
            Err(e) => window.push_notification(e, cx),
        }
        cx.notify();
    }

    fn clear_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match pen_agent::key::delete_stored_key() {
            Ok(()) => {
                self.key_mask = None;
                self.live = None;
            }
            Err(e) => window.push_notification(e, cx),
        }
        cx.notify();
    }

    fn toggle(&mut self, key: String, cx: &mut Context<Self>) {
        if !self.expanded.remove(&key) {
            self.expanded.insert(key);
        }
        cx.notify();
    }

    // ------------------------------------------------------------- render

    fn render_header(&self, p: &Palette, s: &ChatSettings, cx: &mut Context<Self>) -> Div {
        let running = self.model.running;
        let title = if self.model.is_empty() {
            "New chat".to_string()
        } else {
            self.model.title()
        };
        let model_label = format!("{} · {}", engine_short(s.engine), s.model_label());
        div()
            .h(px(metrics::SIDEBAR_HEADER))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.))
            .pb(px(4.))
            .pl(px(16.))
            .pr(px(10.))
            .border_b_1()
            .border_color(header_border(p))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(px(metrics::TEXT_13))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(p.fg.opacity(0.8))
                    .child(title),
            )
            .when(running, |d| {
                d.child(
                    div()
                        .id("chat-running")
                        .flex_none()
                        .size(px(6.))
                        .rounded_full()
                        .bg(rgb(0x60a5fa))
                        .shadow(vec![BoxShadow {
                            color: hsla(0.6, 0.94, 0.68, 0.9),
                            offset: point(px(0.), px(0.)),
                            blur_radius: px(4.),
                            spread_radius: px(0.),
                            inset: false,
                        }])
                        .tooltip(|w, cx| {
                            Tooltip::new("Design generation in progress…").build(w, cx)
                        }),
                )
            })
            .child(div().flex_1())
            .child(
                Button::new("chat-settings")
                    .ghost()
                    .xsmall()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(3.))
                            .text_size(px(12.))
                            .text_color(p.fg.opacity(0.7))
                            .child(model_label)
                            .child(Icon::new(IconName::ChevronDown).size(px(12.))),
                    )
                    .tooltip("Engine and model")
                    .on_click(cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.toggle_settings(cx)
                    })),
            )
            .child(
                Button::new("chat-new")
                    .outline()
                    .xsmall()
                    .icon(Icon::new(IconName::Plus).size(px(14.)))
                    .label("New")
                    .tooltip("New conversation")
                    .on_click(cx.listener(|this, _, window, cx| this.new_conversation(window, cx))),
            )
    }

    fn render_empty(&self, p: &Palette, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .pt(px(6.))
            .child(
                div()
                    .text_size(px(13.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(p.fg)
                    .mb(px(4.))
                    .child("Ask me to design anything"),
            )
            .children(EXAMPLES.iter().enumerate().map(|(i, ex)| {
                let text = ex.to_string();
                div()
                    .id(("chat-example", i))
                    .px(px(10.))
                    .py(px(6.))
                    .rounded(px(10.))
                    .border_1()
                    .border_color(p.fg.opacity(0.1))
                    .text_size(px(12.))
                    .text_color(p.fg.opacity(0.75))
                    .hover(|s| s.bg(p.hover).text_color(p.fg))
                    .cursor_pointer()
                    .child(*ex)
                    .on_click(cx.listener(move |this, _, _, cx| this.send(text.clone(), cx)))
            }))
            .child(
                div()
                    .mt(px(6.))
                    .text_size(px(11.))
                    .text_color(p.fg_muted)
                    .child("The agent edits this document live on the canvas. Each change is one undo step."),
            )
    }

    fn render_user(&self, text: &str, p: &Palette) -> Div {
        let bg = if p.dark {
            Hsla::from(rgb(0x3f3f46)).opacity(0.5)
        } else {
            rgb(0xf4f4f5).into()
        };
        div().flex().justify_end().child(
            div()
                .max_w(relative(0.85))
                .rounded(px(18.))
                .px(px(14.))
                .py(px(10.))
                .text_size(px(13.))
                .line_height(px(20.))
                .bg(bg)
                .text_color(p.fg)
                .child(text.to_string()),
        )
    }

    fn render_text(&self, ix: usize, text: &str, streaming: bool, p: &Palette) -> AnyElement {
        let mut style = TextViewStyle::default().paragraph_gap(rems(0.5));
        style.is_dark = p.dark;
        if p.dark {
            style.highlight_theme = HighlightTheme::default_dark();
        }
        let code_bg: Hsla = if p.dark {
            rgb(0x27272a).into()
        } else {
            rgb(0xf4f4f5).into()
        };
        style = style.code_block(
            StyleRefinement::default()
                .bg(code_bg)
                .rounded(px(6.))
                .p(px(8.)),
        );
        div()
            .w_full()
            .text_size(px(13.))
            .line_height(px(20.))
            .text_color(p.fg.opacity(0.85))
            .child(
                TextView::markdown(("chat-md", ix), text.to_string())
                    .style(style)
                    .selectable(true)
                    .stream_fade(streaming),
            )
            .into_any_element()
    }

    fn render_thinking(
        &self,
        ix: usize,
        text: &str,
        streaming: bool,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = format!("think-{ix}");
        let open = self.expanded.contains(&key);
        let label = if streaming { "Thinking…" } else { "Thought" };
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .id(("chat-think", ix))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .py(px(2.))
                    .rounded(px(4.))
                    .text_size(px(13.))
                    .text_color(p.fg.opacity(0.6))
                    .hover(|s| s.text_color(p.fg))
                    .cursor_pointer()
                    .child(label)
                    .child(
                        Icon::new(if open {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .size(px(14.)),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle(key.clone(), cx))),
            )
            .when(open, |d| {
                d.child(
                    div()
                        .mt(px(2.))
                        .pl(px(10.))
                        .border_l_1()
                        .border_color(p.fg.opacity(0.12))
                        .text_size(px(12.))
                        .line_height(px(18.))
                        .text_color(p.fg.opacity(0.65))
                        .child(text.trim().to_string()),
                )
            })
            .into_any_element()
    }

    fn render_tool(&self, t: &ToolCall, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let open = self.expanded.contains(&t.id);
        let id = t.id.clone();
        let status: AnyElement = match t.state {
            ToolState::Streaming | ToolState::Running => Spinner::new()
                .xsmall()
                .color(p.fg.opacity(0.6))
                .into_any_element(),
            ToolState::Done => Icon::new(IconName::Check)
                .size(px(12.))
                .text_color(rgb(0x4ade80))
                .into_any_element(),
            ToolState::Failed => Icon::new(IconName::CircleX)
                .size(px(12.))
                .text_color(p.danger)
                .into_any_element(),
            ToolState::Cancelled => div()
                .text_size(px(11.))
                .text_color(p.fg_muted)
                .child("stopped")
                .into_any_element(),
        };
        let label =
            if t.name == EXECUTE && t.description.as_deref().is_none_or(|d| d.trim().is_empty()) {
                if t.state.busy() {
                    "Designing"
                } else {
                    "Designed"
                }
                .to_string()
            } else {
                t.label().to_string()
            };
        let mono: SharedString = gpui_kit::component::ActiveTheme::theme(&**cx)
            .mono_font_family
            .clone();
        let section = |title: &'static str| {
            div()
                .px(px(10.))
                .text_size(px(10.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(p.fg.opacity(0.4))
                .child(title.to_uppercase())
        };
        div()
            .flex()
            .flex_col()
            .w_full()
            .text_size(px(13.))
            .child(
                div()
                    .id(SharedString::from(format!("chat-tool-{}", t.id)))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .py(px(3.))
                    .rounded(px(4.))
                    .text_color(p.fg.opacity(0.7))
                    .hover(|s| s.text_color(p.fg))
                    .cursor_pointer()
                    .child(
                        Icon::new(tool_icon(&t.name))
                            .size(px(14.))
                            .text_color(p.fg.opacity(0.5)),
                    )
                    .child(div().min_w_0().truncate().child(label))
                    .child(div().flex_none().child(status))
                    .child(
                        Icon::new(if open {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .size(px(14.))
                        .text_color(p.fg.opacity(0.45)),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle(id.clone(), cx))),
            )
            .when(open, |d| {
                let border = if t.state == ToolState::Failed {
                    Hsla::from(rgb(0x71717a)).opacity(0.5)
                } else if p.dark {
                    Hsla::from(rgb(0x3f3f46)).opacity(0.5)
                } else {
                    rgb(0xe4e4e7).into()
                };
                d.child(
                    div()
                        .mt(px(4.))
                        .py(px(8.))
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .rounded(px(6.))
                        .border_1()
                        .border_color(border)
                        .when_some(t.result.clone(), |d, r| {
                            d.child(section("Result")).child(
                                div()
                                    .id(SharedString::from(format!("chat-tool-res-{}", t.id)))
                                    .px(px(10.))
                                    .max_h(px(150.))
                                    .overflow_y_scroll()
                                    .text_size(px(11.))
                                    .line_height(px(16.))
                                    .text_color(p.fg.opacity(0.75))
                                    .child(r.trim().to_string())
                                    .when(t.images > 0, |d| {
                                        d.child(div().text_color(p.fg_muted).child(format!(
                                            "{} image{} attached",
                                            t.images,
                                            if t.images == 1 { "" } else { "s" }
                                        )))
                                    }),
                            )
                        })
                        .when(!t.code.is_empty(), |d| {
                            d.child(section(if t.name == EXECUTE { "Script" } else { "Input" }))
                                .child(
                                    div()
                                        .id(SharedString::from(format!("chat-tool-code-{}", t.id)))
                                        .px(px(10.))
                                        .max_h(px(160.))
                                        .overflow_y_scroll()
                                        .font_family(mono.clone())
                                        .text_size(px(11.))
                                        .line_height(px(16.))
                                        .text_color(p.fg.opacity(0.7))
                                        .child(t.code.clone()),
                                )
                        }),
                )
            })
            .into_any_element()
    }

    fn render_turn_end(
        &self,
        stop_reason: &str,
        usage: Option<&super::model::TurnUsage>,
        p: &Palette,
        ix: usize,
    ) -> AnyElement {
        let stopped = stop_reason == "interrupted";
        let detail = usage.map(|u| {
            format!(
                "Input {} (cache read {}, cache write {}) · output {}{}",
                u.input_tokens,
                u.cache_read,
                u.cache_write,
                u.output_tokens,
                u.cost_usd
                    .map(|c| format!(" · {}", super::model::money(c)))
                    .unwrap_or_default()
            )
        });
        div()
            .id(("chat-turn-end", ix))
            .flex()
            .items_center()
            .gap(px(8.))
            .text_size(px(11.))
            .text_color(p.fg.opacity(0.4))
            .when(stopped, |d| {
                d.child(
                    div()
                        .text_color(p.fg.opacity(0.55))
                        .child("Agent stopped by user."),
                )
            })
            .when_some(usage, |d, u| d.child(u.summary()))
            .when_some(detail, |d, t| {
                d.tooltip(move |w, cx| Tooltip::new(t.clone()).build(w, cx))
            })
            .into_any_element()
    }

    fn render_error(
        &self,
        kind: ErrorKind,
        message: &str,
        ix: usize,
        last: bool,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let s = settings(cx);
        let who = match kind {
            ErrorKind::NoApiKey => "the Anthropic API",
            ErrorKind::ClaudeNotFound | ErrorKind::NotLoggedIn => "Claude Code",
            _ => match s.engine {
                EngineKind::ClaudeCode => "Claude Code",
                EngineKind::AnthropicApi => "the Anthropic API",
            },
        };
        let body = match kind {
            ErrorKind::ClaudeNotFound => "Claude Code is not installed (the `claude` command was not found). Install it from claude.com/claude-code and log in, or switch the engine to the Anthropic API.".to_string(),
            ErrorKind::NotLoggedIn => "Authentication failed. Run `claude` in a terminal, use /login, then try again.".to_string(),
            ErrorKind::NoApiKey => "No Anthropic API key. Add one in the chat settings; it is kept in the system credential store.".to_string(),
            ErrorKind::Network => format!("Could not reach the server. Check your connection and try again. ({message})"),
            ErrorKind::Other => message.to_string(),
        };
        let amber = if p.dark { rgb(0xeab308) } else { rgb(0xca8a04) };
        let action = |id: &'static str, label: &'static str| {
            div()
                .id((id, ix))
                .px(px(6.))
                .py(px(2.))
                .rounded(px(6.))
                .border_1()
                .border_color(p.fg.opacity(0.15))
                .text_size(px(11.))
                .text_color(p.fg.opacity(0.8))
                .hover(|s| s.bg(p.hover).text_color(p.fg))
                .cursor_pointer()
                .child(label)
        };
        div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .pt(px(6.))
                    .text_size(px(12.))
                    .font_weight(FontWeight::BOLD)
                    .text_color(amber)
                    .child(Icon::new(IconName::TriangleAlert).size(px(13.)))
                    .child(format!("Oops, something went wrong with {who}!")),
            )
            .child(
                div()
                    .text_size(px(12.))
                    .line_height(px(18.))
                    .text_color(p.fg.opacity(0.7))
                    .child(body),
            )
            .when(last, |d| {
                d.child(
                    div()
                        .flex()
                        .gap(px(6.))
                        .mt(px(2.))
                        .when(kind == ErrorKind::ClaudeNotFound, |d| {
                            d.child(action("chat-err-api", "Use the Anthropic API").on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.update_settings(cx, |s| {
                                        s.engine = EngineKind::AnthropicApi
                                    });
                                    this.settings_open = true;
                                    cx.notify();
                                }),
                            ))
                        })
                        .when(kind == ErrorKind::NoApiKey, |d| {
                            d.child(action("chat-err-settings", "Open settings").on_click(
                                cx.listener(|this, _, _, cx| {
                                    if !this.settings_open {
                                        this.toggle_settings(cx);
                                    }
                                }),
                            ))
                        })
                        .when(
                            kind != ErrorKind::ClaudeNotFound && kind != ErrorKind::NoApiKey,
                            |d| {
                                d.child(
                                    action("chat-err-retry", "Try again")
                                        .on_click(cx.listener(|this, _, _, cx| this.retry(cx))),
                                )
                            },
                        ),
                )
            })
            .into_any_element()
    }

    fn render_items(&self, p: &Palette, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let n = self.model.items.len();
        let running = self.model.running;
        let last_error = self.model.last_error().is_some();
        let mut out = Vec::with_capacity(n);
        for (ix, item) in self.model.items.iter().enumerate() {
            let streaming = running && ix + 1 == n;
            out.push(match item {
                Item::User { text, .. } => self.render_user(text, p).into_any_element(),
                Item::Text { text } => self.render_text(ix, text, streaming, p),
                Item::Thinking { text } => self.render_thinking(ix, text, streaming, p, cx),
                Item::Tool(t) => self.render_tool(t, p, cx),
                Item::TurnEnd { stop_reason, usage } => {
                    self.render_turn_end(stop_reason, usage.as_ref(), p, ix)
                }
                Item::Error { error, message } => {
                    let is_last = last_error
                        && self.model.items[ix + 1..]
                            .iter()
                            .all(|i| matches!(i, Item::TurnEnd { .. }));
                    self.render_error(*error, message, ix, is_last, p, cx)
                }
            });
        }
        if running
            && !matches!(
                self.model.items.last(),
                Some(Item::Text { .. } | Item::Thinking { .. })
            )
            && self.model.busy_tools().next().is_none()
        {
            // Waiting for the model: a quiet "Working…".
            out.push(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .text_size(px(13.))
                    .text_color(p.fg.opacity(0.5))
                    .child(Spinner::new().xsmall().color(p.fg.opacity(0.5)))
                    .child("Working…")
                    .into_any_element(),
            );
        }
        out
    }

    fn render_lock(&self, p: &Palette, cx: &mut Context<Self>) -> Option<Div> {
        let lock = self.state.read(cx).doc.stream_lock()?.to_string();
        let ours = self.live.as_ref().is_some_and(|l| l.stream_id == lock);
        let who = if ours {
            "The agent is writing to this document".to_string()
        } else {
            format!(
                "{} is writing to this document",
                lock.split('#').next().unwrap_or("An agent")
            )
        };
        Some(
            div()
                .mx(px(10.))
                .mb(px(6.))
                .px(px(10.))
                .py(px(6.))
                .flex()
                .items_center()
                .gap(px(8.))
                .rounded(px(10.))
                .bg(p.focus.opacity(0.10))
                .text_size(px(11.))
                .text_color(p.fg.opacity(0.75))
                .child(Icon::new(IconName::Lock).size(px(12.)).text_color(p.focus))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(format!("{who}. Your edits wait until it finishes.")),
                )
                .child(
                    div()
                        .id("chat-lock-stop")
                        .px(px(6.))
                        .py(px(1.))
                        .rounded(px(6.))
                        .text_color(p.fg)
                        .hover(|s| s.bg(p.hover))
                        .cursor_pointer()
                        .child("Stop")
                        .on_click(cx.listener(|this, _, _, cx| this.stop_lock(cx))),
                ),
        )
    }

    fn render_composer(&self, p: &Palette, s: &ChatSettings, cx: &mut Context<Self>) -> Div {
        let running = self.model.running;
        let has_text = !self.input.read(cx).value().trim().is_empty();
        let card_bg: Hsla = if p.dark {
            rgb(0x1f1f22).into()
        } else {
            rgb(0xffffff).into()
        };
        let send_bg: Hsla = match (has_text, p.dark) {
            (true, false) => rgb(0x52525b).into(),
            (true, true) => rgb(0xd4d4d8).into(),
            (false, false) => rgb(0xe4e4e7).into(),
            (false, true) => rgb(0x3f3f46).into(),
        };
        let send_fg: Hsla = if has_text {
            if p.dark {
                rgb(0x18181b).into()
            } else {
                rgb(0xffffff).into()
            }
        } else {
            rgb(0xa1a1aa).into()
        };
        div()
            .mx(px(10.))
            .mb(px(10.))
            .px(px(10.))
            .pt(px(8.))
            .pb(px(6.))
            .flex()
            .flex_col()
            .gap(px(4.))
            .rounded(px(14.))
            .border_1()
            .border_color(p.fg.opacity(if p.dark { 0.12 } else { 0.1 }))
            .bg(card_bg)
            .child(
                div()
                    .min_h(px(40.))
                    .text_size(px(13.))
                    .child(Textarea::new(&self.input).appearance(false).bordered(false)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .min_h(px(24.))
                    .gap(px(6.))
                    .child(
                        div()
                            .id("chat-engine-chip")
                            .min_w_0()
                            .truncate()
                            .text_size(px(11.))
                            .text_color(p.fg_muted)
                            .hover(|st| st.text_color(p.fg))
                            .cursor_pointer()
                            .child(format!(
                                "{} · {} · {}",
                                s.engine.label(),
                                s.model_label(),
                                effort_label(s.effort())
                            ))
                            .on_click(cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.toggle_settings(cx)
                            })),
                    )
                    .child(div().flex_1())
                    .when(running, |d| {
                        d.child(
                            div()
                                .id("chat-stop")
                                .size(px(20.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_full()
                                .bg(Hsla::from(rgb(0x71717a)).opacity(0.8))
                                .hover(|s| s.bg(rgb(0x71717a)))
                                .cursor_pointer()
                                .child(div().size(px(8.)).rounded(px(1.5)).bg(rgb(0xffffff)))
                                .tooltip(|w, cx| Tooltip::new("Stop generation").build(w, cx))
                                .on_click(cx.listener(|this, _, _, cx| this.stop(cx))),
                        )
                    })
                    .child(
                        div()
                            .id("chat-send")
                            .size(px(20.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .bg(send_bg)
                            .when(has_text, |d| d.cursor_pointer())
                            .child(
                                Icon::new(IconName::ArrowUp)
                                    .size(px(13.))
                                    .text_color(send_fg),
                            )
                            .tooltip(move |w, cx| {
                                Tooltip::new(if running {
                                    "Send to the running agent"
                                } else {
                                    "Send message"
                                })
                                .build(w, cx)
                            })
                            .on_click(cx.listener(|this, _, window, cx| {
                                let text = this.input.read(cx).value().to_string();
                                if !text.trim().is_empty() {
                                    this.input.update(cx, |s, cx| s.set_value("", window, cx));
                                    this.send(text, cx);
                                }
                            })),
                    ),
            )
    }

    fn render_settings(
        &self,
        p: &Palette,
        s: &ChatSettings,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let label = |t: &'static str| {
            div()
                .text_size(px(11.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(p.fg_muted)
                .child(t)
        };
        let seg_bg = p.control;
        let seg = |id: SharedString, text: String, active: bool| {
            div()
                .id(id)
                .flex_1()
                .h(px(24.))
                .px(px(6.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(5.))
                .text_size(px(11.))
                .text_color(if active { p.fg } else { p.fg_muted })
                .when(active, |d| d.bg(p.segment_active).shadow_xs())
                .when(!active, |d| d.hover(|s| s.text_color(p.fg)))
                .cursor_pointer()
                .child(text)
        };
        let row = |children: Vec<AnyElement>| {
            div()
                .flex()
                .gap(px(2.))
                .p(px(2.))
                .rounded(px(7.))
                .bg(seg_bg)
                .children(children)
        };
        let engines: Vec<AnyElement> = [EngineKind::ClaudeCode, EngineKind::AnthropicApi]
            .into_iter()
            .map(|e| {
                seg(
                    format!("chat-eng-{e:?}").into(),
                    e.label().into(),
                    s.engine == e,
                )
                .on_click(
                    cx.listener(move |this, _, _, cx| this.update_settings(cx, |s| s.engine = e)),
                )
                .into_any_element()
            })
            .collect();
        let models: Vec<AnyElement> = match s.engine {
            EngineKind::ClaudeCode => CLI_MODELS
                .iter()
                .map(|(id, name)| {
                    let id = id.to_string();
                    let active = s.cli_model == id;
                    seg(format!("chat-cm-{id}").into(), name.to_string(), active)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let id = id.clone();
                            this.update_settings(cx, |s| s.cli_model = id)
                        }))
                        .into_any_element()
                })
                .collect(),
            EngineKind::AnthropicApi => API_MODELS
                .iter()
                .map(|(id, name)| {
                    let id = id.to_string();
                    let active = s.api_model == id;
                    seg(format!("chat-am-{id}").into(), name.to_string(), active)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let id = id.clone();
                            this.update_settings(cx, |s| s.api_model = id)
                        }))
                        .into_any_element()
                })
                .collect(),
        };
        let efforts: Vec<AnyElement> =
            EFFORTS
                .iter()
                .map(|(e, name)| {
                    let e = *e;
                    let short = if *name == "Extra high" { "XHigh" } else { name };
                    seg(
                        format!("chat-ef-{}", e.as_str()).into(),
                        short.to_string(),
                        s.effort() == e,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.update_settings(cx, |s| s.set_effort(e))
                    }))
                    .into_any_element()
                })
                .collect();
        let note = |t: String| {
            div()
                .text_size(px(11.))
                .line_height(px(15.))
                .text_color(p.fg_muted)
                .child(t)
        };
        let switch_row = |text: &'static str, sw: Switch| {
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap(px(8.))
                .text_size(px(12.))
                .text_color(p.fg)
                .child(text)
                .child(sw.xsmall())
        };
        let mut panel = div()
            .id("chat-settings-popover")
            .occlude()
            .absolute()
            .top(px(metrics::SIDEBAR_HEADER - 2.))
            .right(px(10.))
            .w(px(290.))
            .p(px(12.))
            .flex()
            .flex_col()
            .gap(px(8.))
            .rounded(px(12.))
            .border_1()
            .border_color(p.border)
            .bg(p.popover)
            .shadow_lg()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(label("Engine"))
            .child(row(engines))
            .child(label("Model"))
            .child(row(models));
        if s.engine == EngineKind::AnthropicApi {
            panel = panel.child(Input::new(&self.custom_model).xsmall().appearance(true));
        }
        panel = panel.child(label("Effort")).child(row(efforts));
        match s.engine {
            EngineKind::ClaudeCode => {
                let user = s.use_user_settings;
                let web = s.allow_web;
                panel = panel
                    .child(switch_row(
                        "Use my Claude Code settings",
                        Switch::new("chat-user-settings").checked(user).on_click(cx.listener(
                            |this, v: &bool, _, cx| {
                                let v = *v;
                                this.update_settings(cx, |s| s.use_user_settings = v)
                            },
                        )),
                    ))
                    .child(note(
                        "Off: the chat ignores your own hooks, plugins and settings (--setting-sources \"\"). The chat runs your installed Claude Code, on your account.".into(),
                    ))
                    .child(switch_row(
                        "Allow web search and fetch",
                        Switch::new("chat-web").checked(web).on_click(cx.listener(
                            |this, v: &bool, _, cx| {
                                let v = *v;
                                this.update_settings(cx, |s| s.allow_web = v)
                            },
                        )),
                    ));
            }
            EngineKind::AnthropicApi => {
                panel = panel.child(label("API key"));
                panel =
                    match &self.key_mask {
                        Some(mask) => panel.child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .child(
                                    div()
                                        .flex_1()
                                        .text_size(px(12.))
                                        .text_color(p.fg)
                                        .child(format!("Saved: {mask}")),
                                )
                                .child(
                                    Button::new("chat-key-clear")
                                        .outline()
                                        .xsmall()
                                        .label("Clear")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.clear_key(window, cx)
                                        })),
                                ),
                        ),
                        None => panel.child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .child(div().flex_1().child(Input::new(&self.key_input).xsmall()))
                                .child(
                                    Button::new("chat-key-save")
                                        .outline()
                                        .xsmall()
                                        .label("Save")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.save_key(window, cx)
                                        })),
                                ),
                        ),
                    };
                let env =
                    std::env::var(pen_agent::key::ENV_VAR).is_ok_and(|v| !v.trim().is_empty());
                panel = panel.child(note(format!(
                    "The key is kept in the system credential store, never in OpenPen's files.{} API calls are billed per token to your Anthropic Console account.",
                    if env && self.key_mask.is_none() {
                        " ANTHROPIC_API_KEY is set and will be used."
                    } else {
                        ""
                    }
                )));
            }
        }
        panel
    }
}

fn engine_short(e: EngineKind) -> &'static str {
    match e {
        EngineKind::ClaudeCode => "Claude Code",
        EngineKind::AnthropicApi => "API",
    }
}

fn tool_icon(name: &str) -> IconName {
    match name {
        EXECUTE => IconName::Pencil,
        "get_app_state" | "get_style" => IconName::Eye,
        "start_task" | "read_skill" => IconName::BookOpen,
        "save" => IconName::Save,
        "WebSearch" | "WebFetch" => IconName::Globe,
        _ => IconName::Wrench,
    }
}

fn header_border(p: &Palette) -> Hsla {
    if p.dark {
        Hsla::from(rgb(0x3f3f46)).opacity(0.5)
    } else {
        rgb(0xe4e4e7).into()
    }
}

impl Render for ChatPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.pal();
        let s = settings(cx);
        // Follow the tail while the user has not scrolled up.
        if self.model.revision != self.last_rev {
            self.last_rev = self.model.revision;
            if self.follow {
                self.scroll.scroll_to_bottom();
            }
        } else {
            let (off, max) = (self.scroll.offset(), self.scroll.max_offset());
            self.follow = f32::from(-off.y) >= f32::from(max.y) - 24.0;
        }
        let body = if self.model.is_empty() {
            self.render_empty(&p, cx).into_any_element()
        } else {
            div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .children(self.render_items(&p, cx))
                .into_any_element()
        };
        let settings_open = self.settings_open;
        div()
            .id("chat-panel")
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .text_color(p.card_fg)
            .child(self.render_header(&p, &s, cx))
            .child(
                div()
                    .id("chat-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .on_scroll(cx.listener(|this, _, _, cx| {
                        this.last_rev = this.model.revision;
                        cx.notify();
                    }))
                    .child(div().px(px(14.)).pt(px(12.)).pb(px(14.)).child(body)),
            )
            .when_some(self.render_lock(&p, cx), |d, l| d.child(l))
            .child(self.render_composer(&p, &s, cx))
            .when(settings_open, |d| {
                d.child(
                    div()
                        .id("chat-settings-scrim")
                        .absolute()
                        .inset_0()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.settings_open = false;
                                cx.notify();
                            }),
                        ),
                )
                .child(self.render_settings(&p, &s, cx))
            })
    }
}
