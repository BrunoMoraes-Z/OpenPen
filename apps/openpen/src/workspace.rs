//! Window root: Pen tab bar (islands with Home + document tabs + window
//! controls) over the active editor or the Home dashboard.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, ThemeMode, WindowExt};
use gpui_kit::prelude::FluentBuilder;
use std::path::{Path, PathBuf};
use std::time::Instant;

use gpui_kit::*;

use crate::actions::*;
use crate::editor::Editor;
use crate::editor_state::EditorState;
use crate::home::{HomeEvent, HomeView};
use crate::scene_document::SceneDocument;
use crate::theme::{self, Palette, PaletteExt, island, metrics};
use crate::{fonts, recent, save};

pub struct DocTab {
    pub state: Entity<EditorState>,
    pub editor: Entity<Editor>,
}

/// The window's workspace (menu items built outside it reach it here).
pub struct WorkspaceRef(pub WeakEntity<Workspace>);

impl Global for WorkspaceRef {}

pub struct Workspace {
    tabs: Vec<DocTab>,
    /// `None` = Home.
    active: Option<usize>,
    focus: FocusHandle,
    /// Theme forced by the user (otherwise follow the OS).
    theme_override: Option<ThemeMode>,
    untitled: usize,
    /// Set once the user agreed to discard unsaved changes on quit.
    force_close: bool,
    /// Last document tab that was active (agent calls without `uri` go to
    /// it while Home is shown).
    last_doc: Option<WeakEntity<EditorState>>,
    /// The Home dashboard (kept alive so thumbnails and state survive tab
    /// switches).
    home: Entity<HomeView>,
    _subs: Vec<Subscription>,
}

/// Appends a line to `%TEMP%/openpen.log` (release builds have no console).
pub fn log(line: &str) {
    use std::io::Write;
    eprintln!("openpen: {line}");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(std::env::temp_dir().join("openpen.log"))
    {
        let _ = writeln!(f, "{line}");
    }
}

fn display_name(path: &Path) -> SharedString {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Untitled".into())
        .into()
}

/// `canonicalize` without the Windows verbatim prefix (`\\?\`).
pub fn canonical(path: &Path) -> PathBuf {
    let c = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let s = c.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
        _ => c,
    }
}

/// `.pen` files named on the command line.
pub fn cli_paths(args: impl IntoIterator<Item = String>) -> Vec<PathBuf> {
    args.into_iter()
        .skip(1)
        .filter(|a| !a.starts_with("--"))
        .map(PathBuf::from)
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("pen")))
        .collect()
}

impl Workspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let home = cx.new(|cx| HomeView::new(window, cx));
        let subs = vec![
            cx.subscribe_in(
                &home,
                window,
                |this, _, ev: &HomeEvent, window, cx| match ev {
                    HomeEvent::NewFile => this.new_file(window, cx),
                    HomeEvent::OpenDialog => this.open_dialog(window, cx),
                    HomeEvent::Open(p) => this.open_path(p.clone(), window, cx),
                },
            ),
            cx.observe_window_appearance(window, |this, window, cx| {
                if this.theme_override.is_none() {
                    theme::apply(theme::mode_for(window.appearance()), cx);
                }
            }),
            // Back from another app: image files may have changed on disk.
            cx.observe_window_activation(window, |this, window, cx| {
                if window.is_window_active() {
                    for t in &this.tabs {
                        t.state.update(cx, |s, cx| s.reload_changed_assets(cx));
                    }
                }
            }),
        ];
        let theme_override = cx.try_global::<theme::ThemeOverride>().and_then(|o| o.0);
        let weak = cx.entity().downgrade();
        cx.set_global(WorkspaceRef(weak.clone()));
        window.on_window_should_close(cx, move |window, cx| {
            weak.update(cx, |this, cx| this.should_close(window, cx))
                .unwrap_or(true)
        });
        let mut this = Self {
            tabs: Vec::new(),
            active: None,
            focus: cx.focus_handle(),
            theme_override,
            untitled: 0,
            force_close: false,
            last_doc: None,
            home,
            _subs: subs,
        };
        for path in cli_paths(std::env::args()) {
            this.open_path(path, window, cx);
        }
        if this.tabs.is_empty() {
            this.activate(None, window, cx);
        }
        this
    }

    fn add_tab(&mut self, state: Entity<EditorState>, window: &mut Window, cx: &mut Context<Self>) {
        let editor = cx.new(|cx| Editor::new(state.clone(), window, cx));
        self._subs.push(cx.observe(&state, |_, _, cx| cx.notify()));
        self.tabs.push(DocTab { state, editor });
        let ix = self.tabs.len() - 1;
        self.activate(Some(ix), window, cx);
    }

    fn tab_of(&self, state: &Entity<EditorState>) -> Option<usize> {
        self.tabs
            .iter()
            .position(|t| t.state.entity_id() == state.entity_id())
    }

    /// Opens `path` in a new tab (or activates it). Reading, parsing and the
    /// first layout run on the background executor.
    pub fn open_path(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let canon = canonical(&path);
        if let Some(ix) = self.tabs.iter().position(|t| {
            t.state
                .read(cx)
                .path
                .as_ref()
                .is_some_and(|p| canonical(p) == canon)
        }) {
            self.activate(Some(ix), window, cx);
            return;
        }
        let state = cx.new(|_| {
            let mut s = EditorState::new(
                SceneDocument::placeholder(),
                display_name(&path),
                Some(canon.clone()),
            );
            s.loading = true;
            s
        });
        self.add_tab(state.clone(), window, cx);
        let fonts = fonts::fonts(cx).data.clone();
        let t0 = Instant::now();
        let load_path = canon.clone();
        let task = cx.background_spawn(async move { SceneDocument::load(&load_path, &fonts) });
        cx.spawn_in(window, async move |this, cx| {
            let res = task.await;
            let opened = res.is_ok();
            let _ = this.update_in(cx, |this, window, cx| match res {
                Ok((doc, st)) => {
                    log(&format!(
                        "opened {} in {:.1?} (read {:.1?}, parse+migrate+scene {:.1?}, fonts {:.1?}, layout+display list {:.1?}, {} nodes)",
                        canon.display(),
                        t0.elapsed(),
                        st.read,
                        st.parse,
                        st.fonts,
                        st.layout,
                        st.nodes
                    ));
                    let t1 = Instant::now();
                    state.update(cx, |s, cx| s.replace_doc(doc, cx));
                    log(&format!("  swapped into the UI in {:.1?}", t1.elapsed()));
                    recent::add(&canon, cx);
                }
                Err(e) => {
                    if let Some(ix) = this.tab_of(&state) {
                        this.remove_tab(ix, window, cx);
                    }
                    window.push_notification(
                        format!("Could not open {}: {e}", canon.display()),
                        cx,
                    );
                }
            });
            // Image fills decode in the background after the first frame: swap
            // their chunks in as they finish.
            let mut pending = opened;
            while pending {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(30))
                    .await;
                pending = state.update(cx, |s, cx| {
                    if s.doc.poll_assets() {
                        cx.notify();
                    }
                    s.doc.has_pending_assets()
                });
            }
        })
        .detach();
    }

    fn open_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Open".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                let _ = this.update_in(cx, |this, window, cx| {
                    for p in paths {
                        this.open_path(p, window, cx);
                    }
                });
            }
        })
        .detach();
    }

    /// Opens the `.pen` files of a drop on the window.
    fn open_dropped(&mut self, paths: &ExternalPaths, window: &mut Window, cx: &mut Context<Self>) {
        let pens: Vec<PathBuf> = paths
            .paths()
            .iter()
            .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("pen")))
            .cloned()
            .collect();
        if pens.is_empty() {
            window.push_notification("OpenPen can only open .pen files.", cx);
            return;
        }
        for p in pens {
            self.open_path(p, window, cx);
        }
    }

    fn activate(&mut self, ix: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        self.active = ix.filter(|&i| i < self.tabs.len());
        match self.active {
            Some(i) => {
                self.last_doc = Some(self.tabs[i].state.downgrade());
                let h = self.tabs[i].editor.read(cx).canvas_focus(cx);
                h.focus(window, cx);
                let name = self.tabs[i].state.read(cx).name.clone();
                window.set_window_title(&format!("{name} — OpenPen"));
            }
            None => {
                let h = self.home.read(cx).focus_handle().clone();
                h.focus(window, cx);
                self.home.update(cx, |h, cx| h.refresh(cx));
                window.set_window_title("OpenPen");
            }
        }
        cx.notify();
    }

    fn remove_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        let tab = self.tabs.remove(ix);
        crate::agent::hub::doc_closing(tab.state.entity_id(), cx);
        let next = match self.active {
            Some(a) if a == ix => (!self.tabs.is_empty()).then(|| ix.min(self.tabs.len() - 1)),
            Some(a) if a > ix => Some(a - 1),
            other => other,
        };
        self.activate(next, window, cx);
    }

    /// Closes a tab, asking to save unsaved changes first.
    fn close(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(ix) else { return };
        let state = tab.state.clone();
        // Closing a tab rolls back an agent stream on it (before asking to save).
        crate::agent::hub::doc_closing(state.entity_id(), cx);
        if !state.read(cx).is_dirty() {
            self.remove_tab(ix, window, cx);
            return;
        }
        let name = state.read(cx).name.clone();
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("Do you want to save the changes you made to {name}?"),
            Some("Your changes will be lost if you don't save them."),
            &["Save", "Don't Save", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            let Ok(choice) = answer.await else { return };
            let _ = this.update_in(cx, |this, window, cx| match choice {
                0 => this.save(&state, false, true, window, cx),
                1 => {
                    if let Some(ix) = this.tab_of(&state) {
                        this.remove_tab(ix, window, cx);
                    }
                }
                _ => {}
            });
        })
        .detach();
    }

    fn should_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.force_close {
            return true;
        }
        let dirty: Vec<SharedString> = self
            .tabs
            .iter()
            .filter(|t| t.state.read(cx).is_dirty())
            .map(|t| t.state.read(cx).name.clone())
            .collect();
        if dirty.is_empty() {
            return true;
        }
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!(
                "{} document(s) have unsaved changes: {}",
                dirty.len(),
                dirty.join(", ")
            ),
            Some("Quit without saving?"),
            &["Quit Without Saving", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(0) = answer.await {
                let _ = this.update_in(cx, |this, window, _| {
                    this.force_close = true;
                    window.remove_window();
                });
            }
        })
        .detach();
        false
    }

    /// Saves a tab (Save As when it has no path or `save_as`), atomically, on
    /// the background executor; optionally closes it afterwards.
    fn save(
        &mut self,
        state: &Entity<EditorState>,
        save_as: bool,
        close_after: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let s = state.read(cx);
        if s.loading {
            return;
        }
        if s.doc.stream_lock().is_some() {
            // An agent's streamed `execute` is half applied: never write that.
            // Save once it is committed (or rolled back / timed out).
            window.push_notification(
                "An AI agent is writing to this document; it will be saved when it finishes.",
                cx,
            );
            let state = state.clone();
            cx.spawn_in(window, async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(100))
                        .await;
                    if !state.read_with(cx, |s, _| s.doc.stream_lock().is_some()) {
                        break;
                    }
                }
                let _ = this.update_in(cx, |this, window, cx| {
                    if this.tab_of(&state).is_some() {
                        this.save(&state, save_as, close_after, window, cx);
                    }
                });
            })
            .detach();
            return;
        }
        let Some(path) = s.path.clone().filter(|_| !save_as) else {
            let dir = s
                .path
                .as_ref()
                .and_then(|p| p.parent().map(Path::to_path_buf))
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_default();
            let mut suggested = s.name.to_string();
            if !suggested.ends_with(".pen") {
                suggested.push_str(".pen");
            }
            let rx = cx.prompt_for_new_path(&dir, Some(&suggested));
            let state = state.clone();
            cx.spawn_in(window, async move |this, cx| {
                if let Ok(Ok(Some(mut p))) = rx.await {
                    if p.extension().is_none() {
                        p.set_extension("pen");
                    }
                    let _ = this.update_in(cx, |this, window, cx| {
                        state.update(cx, |s, cx| {
                            s.path = Some(p.clone());
                            s.name = display_name(&p);
                            cx.notify();
                        });
                        this.save(&state, false, close_after, window, cx);
                        this.refresh_title(window, cx);
                    });
                }
            })
            .detach();
            return;
        };
        let task = self.write_doc(state, path.clone(), cx);
        let state = state.clone();
        cx.spawn_in(window, async move |this, cx| {
            let res = task.await;
            let _ = this.update_in(cx, |this, window, cx| match res {
                Ok(()) => {
                    if close_after && let Some(ix) = this.tab_of(&state) {
                        this.remove_tab(ix, window, cx);
                    }
                }
                Err(e) => {
                    window.push_notification(format!("Could not save {}: {e}", path.display()), cx);
                }
            });
        })
        .detach();
    }

    /// Serializes a document on the UI thread and writes it atomically on the
    /// background executor; marks it saved (at the undo step it had when
    /// serialized) and adds it to the recent files.
    fn write_doc(
        &mut self,
        state: &Entity<EditorState>,
        path: PathBuf,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        let t0 = Instant::now();
        let s = state.read(cx);
        let data = s.doc.save_string();
        let step = s.doc.current_step();
        let serialize = t0.elapsed();
        let state = state.clone();
        let target = path.clone();
        let task = cx.background_spawn(async move { save::write_atomic(&target, data.as_bytes()) });
        cx.spawn(async move |_, cx| match task.await {
            Ok(()) => {
                log(&format!(
                    "saved {} (serialize on UI thread {:.1?}, total {:.1?})",
                    path.display(),
                    serialize,
                    t0.elapsed()
                ));
                state.update(cx, |s, cx| {
                    s.doc.mark_saved_at(step);
                    cx.notify();
                });
                cx.update(|cx| recent::add(&path, cx));
                Ok(())
            }
            Err(e) => Err(e.to_string()),
        })
    }

    // ------------------------------------------------------------ agents

    /// Document tabs as seen by the agent router (canonical paths).
    pub fn tab_infos(&self, cx: &App) -> Vec<crate::agent::route::TabInfo<Entity<EditorState>>> {
        self.tabs
            .iter()
            .map(|t| crate::agent::route::TabInfo {
                id: t.state.clone(),
                path: t.state.read(cx).path.as_deref().map(canonical),
            })
            .collect()
    }

    /// The active document, or the last active one while Home is shown.
    pub fn focused_doc(&self) -> Option<Entity<EditorState>> {
        match self.active {
            Some(i) => Some(self.tabs[i].state.clone()),
            None => self
                .last_doc
                .as_ref()
                .and_then(|w| w.upgrade())
                .filter(|s| self.tab_of(s).is_some()),
        }
    }

    pub fn doc_states(&self) -> Vec<Entity<EditorState>> {
        self.tabs.iter().map(|t| t.state.clone()).collect()
    }

    /// The tab showing `path` (canonical), if any.
    pub fn doc_for_path(&self, path: &Path, cx: &App) -> Option<Entity<EditorState>> {
        let canon = canonical(path);
        self.tabs
            .iter()
            .find(|t| {
                t.state
                    .read(cx)
                    .path
                    .as_deref()
                    .is_some_and(|p| canonical(p) == canon)
            })
            .map(|t| t.state.clone())
    }

    /// Visible world rect of a document's canvas.
    pub fn visible_world(&self, state: &Entity<EditorState>, cx: &App) -> Option<pen_render::Rect> {
        let ix = self.tab_of(state)?;
        let editor = self.tabs[ix].editor.read(cx);
        Some(editor.canvas_host().read(cx).visible_world(cx))
    }

    /// (name, path, focused) of every open document.
    pub fn open_documents(&self, cx: &App) -> Vec<(SharedString, Option<PathBuf>, bool)> {
        let focused = self.focused_doc().map(|s| s.entity_id());
        self.tabs
            .iter()
            .map(|t| {
                let s = t.state.read(cx);
                (
                    s.name.clone(),
                    s.path.clone(),
                    Some(t.state.entity_id()) == focused,
                )
            })
            .collect()
    }

    /// `save` tool: writes a document that already has a file (never shows a
    /// dialog); resolves to the path written.
    pub fn save_for_agent(
        &mut self,
        state: &Entity<EditorState>,
        cx: &mut Context<Self>,
    ) -> Task<Result<PathBuf, String>> {
        let Some(path) = state.read(cx).path.clone() else {
            return Task::ready(Err(
                "This document has never been saved, so it has no file yet. Ask the user to save it (File > Save As) first."
                    .into(),
            ));
        };
        let task = self.write_doc(state, path.clone(), cx);
        cx.spawn(async move |_, _| task.await.map(|()| path))
    }

    fn save_active(&mut self, save_as: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(a) = self.active {
            let state = self.tabs[a].state.clone();
            self.save(&state, save_as, false, window, cx);
        }
    }

    fn refresh_title(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let a = self.active;
        self.activate(a, window, cx);
    }

    fn new_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.untitled += 1;
        let name = if self.untitled == 1 {
            "Untitled".to_string()
        } else {
            format!("Untitled {}", self.untitled)
        };
        let state = cx.new(|_| {
            let mut s = EditorState::new(SceneDocument::placeholder(), name, None);
            s.loading = true;
            s
        });
        self.add_tab(state.clone(), window, cx);
        let fonts = fonts::fonts(cx).data.clone();
        let task = cx.background_spawn(async move { SceneDocument::empty(&fonts) });
        cx.spawn_in(window, async move |_, cx| {
            let doc = task.await;
            state.update(cx, |s, cx| s.replace_doc(doc, cx));
        })
        .detach();
    }

    fn step(&mut self, d: isize, window: &mut Window, cx: &mut Context<Self>) {
        // Home counts as position 0 in the cycle.
        let n = self.tabs.len() as isize + 1;
        let cur = self.active.map(|a| a as isize + 1).unwrap_or(0);
        let next = (cur + d).rem_euclid(n);
        self.activate(
            if next == 0 {
                None
            } else {
                Some(next as usize - 1)
            },
            window,
            cx,
        );
    }

    fn toggle_theme(&mut self, cx: &mut Context<Self>) {
        let mode = if cx.pal().dark {
            ThemeMode::Light
        } else {
            ThemeMode::Dark
        };
        self.theme_override = Some(mode);
        theme::apply(mode, cx);
    }

    fn not_yet(what: &str, window: &mut Window, cx: &mut App) {
        window.push_notification(format!("{what} is not available yet."), cx);
    }

    // ------------------------------------------------------------- tab bar

    fn pill(id: impl Into<ElementId>, p: &Palette) -> Stateful<Div> {
        let hover = p.hover;
        let fg = p.fg;
        div()
            .id(id.into())
            .h(px(metrics::PILL))
            .min_w(px(metrics::PILL))
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .rounded_full()
            .text_size(px(metrics::TEXT_XS))
            .text_color(p.fg_muted)
            .hover(move |s| s.bg(hover).text_color(fg))
    }

    fn render_tab(&self, ix: usize, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let tab = &self.tabs[ix];
        let active = self.active == Some(ix);
        let (dirty, name) = {
            let s = tab.state.read(cx);
            (s.is_dirty(), s.name.clone())
        };
        let prev_active = ix > 0 && self.active == Some(ix - 1);
        let group: SharedString = format!("tab-{ix}").into();
        let (hover, fg) = (p.tab_active, p.fg);
        div()
            .id(("doc-tab", ix))
            .group(group.clone())
            .relative()
            .h(px(metrics::PILL))
            .min_w(px(metrics::TAB_MIN_WIDTH))
            .max_w(px(metrics::TAB_MAX_WIDTH))
            .flex_1()
            .flex()
            .items_center()
            .gap(px(4.))
            .pl(px(2.))
            .pr(px(12.))
            .rounded_full()
            .text_size(px(metrics::TEXT_XS))
            .font_weight(FontWeight::MEDIUM)
            .text_color(if active { p.fg } else { p.fg_muted })
            .when(active, |d| d.bg(p.tab_active))
            .when(!active, |d| d.hover(move |s| s.bg(hover).text_color(fg)))
            // 1x16 divider between two inactive tabs.
            .when(ix > 0 && !active && !prev_active, |d| {
                d.child(
                    div()
                        .absolute()
                        .left(px(-1.5))
                        .top(px(6.))
                        .w(px(1.))
                        .h(px(16.))
                        .bg(p.divider)
                        .group_hover(group.clone(), |s| s.invisible()),
                )
            })
            .child(
                div()
                    .size(px(24.))
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .child(
                        Icon::new(IconName::File)
                            .size(px(12.))
                            .when(!active, |d| d.opacity(0.5)),
                    ),
            )
            .child(div().flex_1().min_w_0().truncate().child(name))
            .when(dirty, |d| {
                d.child(
                    div()
                        .ml(px(6.))
                        .size(px(8.))
                        .flex_none()
                        .rounded_full()
                        .bg(p.fg_muted)
                        .group_hover(group.clone(), |s| s.invisible()),
                )
            })
            .child(
                div()
                    .id(("tab-close", ix))
                    .absolute()
                    .right(px(2.))
                    .size(px(24.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .invisible()
                    .group_hover(group, |s| s.visible())
                    .hover(move |s| s.bg(hover))
                    .child(Icon::new(IconName::X).size(px(12.)))
                    .tooltip(|w, cx| {
                        Tooltip::new("Close Tab")
                            .action(&CloseTab, Some(EDITOR_CTX))
                            .build(w, cx)
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.close(ix, window, cx);
                    })),
            )
            .on_click(cx.listener(move |this, _, window, cx| this.activate(Some(ix), window, cx)))
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(move |this, _, window, cx| this.close(ix, window, cx)),
            )
    }

    fn render_tab_bar(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.pal();
        let home_active = self.active.is_none();
        let mut tabs = div()
            .flex()
            .items_center()
            .gap(px(2.))
            .min_w_0()
            .overflow_hidden();
        for ix in 0..self.tabs.len() {
            tabs = tabs.child(self.render_tab(ix, &p, cx));
        }
        let left = island(&p)
            .id("tabs-left")
            .occlude()
            .h(px(metrics::TAB_ISLAND_HEIGHT))
            .min_w_0()
            .flex_shrink(1.)
            .flex()
            .items_center()
            .pl(px(if cfg!(target_os = "macos") { 7. } else { 4. }))
            .pr(px(7.))
            .when(cfg!(target_os = "macos"), |d| {
                d.child(div().w(px(metrics::TRAFFIC_LIGHTS)).flex_none())
            })
            .when(!cfg!(target_os = "macos"), |d| d.child(app_menu_button(&p)))
            .child(
                Self::pill("wordmark", &p)
                    .px(px(6.))
                    .text_size(px(13.))
                    .font_weight(FontWeight::BOLD)
                    .text_color(p.fg)
                    .child("openpen")
                    .tooltip(|w, cx| Tooltip::new("Toggle light/dark mode").build(w, cx))
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_theme(cx))),
            )
            .child(
                Self::pill("home", &p)
                    .ml(px(6.))
                    .when(home_active, |d| {
                        d.bg(p.tab_active)
                            .text_color(p.fg)
                            .border_1()
                            .border_color(if p.dark { p.input } else { p.border })
                    })
                    .child(Icon::new(IconName::House).size(px(14.)))
                    .tooltip(|w, cx| {
                        Tooltip::new("Home")
                            .action(&ShowHome, Some(EDITOR_CTX))
                            .build(w, cx)
                    })
                    .on_click(cx.listener(|this, _, window, cx| this.activate(None, window, cx))),
            )
            .child(
                div()
                    .mx(px(6.))
                    .w(px(1.))
                    .h(px(16.))
                    .flex_none()
                    .bg(p.divider),
            )
            .child(tabs)
            .child(
                Self::pill("new-tab", &p)
                    .ml(px(6.))
                    .when(self.tabs.is_empty(), |d| {
                        d.gap(px(6.))
                            .pl(px(8.))
                            .pr(px(10.))
                            .child(Icon::new(IconName::Plus).size(px(14.)))
                            .child("New")
                    })
                    .when(!self.tabs.is_empty(), |d| {
                        d.child(Icon::new(IconName::Plus).size(px(14.)))
                    })
                    .tooltip(|w, cx| {
                        Tooltip::new("New File")
                            .action(&NewFile, Some(EDITOR_CTX))
                            .build(w, cx)
                    })
                    .on_click(cx.listener(|this, _, window, cx| this.new_file(window, cx))),
            );
        let action_btn = |id: &'static str, icon: IconName, tip: &'static str| {
            Self::pill(id, &p)
                .child(Icon::new(icon).size(px(14.)))
                .tooltip(move |w, cx| Tooltip::new(tip).build(w, cx))
                .on_click(move |_, w, cx| Self::not_yet(tip, w, cx))
        };
        let right = island(&p)
            .id("tabs-right")
            .occlude()
            .h(px(metrics::TAB_ISLAND_HEIGHT))
            .flex()
            .flex_none()
            .items_center()
            .gap_px()
            .px(px(7.))
            .when(!home_active, |d| {
                d.child(
                    Self::pill("share", &p)
                        .px(px(10.))
                        .gap(px(6.))
                        .child(Icon::new(IconName::Share).size(px(14.)))
                        .child("Share")
                        .on_click(|_, w, cx| Self::not_yet("Sharing", w, cx)),
                )
                .child(action_btn("goodies", IconName::Gift, "Goodies"))
            })
            .child(crate::agent::ui::agents_pill(&p, cx))
            .child(action_btn("settings", IconName::Settings, "Settings"))
            .when(!home_active, |d| {
                d.child(action_btn("present", IconName::Play, "Present"))
                    .child(action_btn(
                        "browser",
                        IconName::Globe,
                        "Open a webpage and reproduce it on the canvas",
                    ))
            });
        let maximized = window.is_maximized();
        // A solid app bar spanning the whole width. It occludes the canvas
        // below so a press on its empty space never starts a canvas gesture
        // (marquee), and the empty space is the window's drag/caption area
        // (move, double-click to maximize, Aero snap).
        div()
            .id("tab-bar")
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h(px(metrics::TAB_BAR_HEIGHT + metrics::ISLAND_INSET / 2.0))
            .pt(px(metrics::ISLAND_INSET))
            .px(px(metrics::ISLAND_INSET))
            .bg(p.canvas)
            .border_b_1()
            .border_color(p.border)
            .occlude()
            .flex()
            .items_start()
            .gap(px(8.))
            .window_control_area(WindowControlArea::Drag)
            .child(left)
            .child(div().flex_1().h(px(metrics::TAB_ISLAND_HEIGHT)))
            .child(right)
            .when(
                cfg!(target_os = "windows") || cfg!(target_os = "linux"),
                |d| d.child(window_controls(maximized, &p)),
            )
    }
}

/// Min / max / close as a third island (Pen uses the native frame on
/// Windows; OpenPen draws client-side controls in the same pill style).
fn window_controls(maximized: bool, p: &Palette) -> impl IntoElement {
    let btn =
        |id: &'static str, icon: IconName, area: WindowControlArea, close: bool, p: &Palette| {
            let hover = if close {
                Hsla::from(rgb(0xe81123))
            } else {
                p.hover
            };
            let fg = p.fg;
            div()
                .id(id)
                .size(px(metrics::PILL))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .text_color(p.fg_muted)
                .hover(move |s| {
                    s.bg(hover)
                        .text_color(if close { Hsla::from(rgb(0xffffff)) } else { fg })
                })
                .window_control_area(area)
                .when(cfg!(target_os = "linux"), |d| {
                    d.on_click(move |_, w, _| match area {
                        WindowControlArea::Min => w.minimize_window(),
                        WindowControlArea::Max => w.zoom_window(),
                        _ => w.remove_window(),
                    })
                })
                .child(Icon::new(icon).size(px(14.)))
        };
    island(p)
        .id("window-controls")
        .occlude()
        .h(px(metrics::TAB_ISLAND_HEIGHT))
        .flex()
        .flex_none()
        .items_center()
        .gap_px()
        .px(px(7.))
        .child(btn(
            "win-min",
            IconName::Minus,
            WindowControlArea::Min,
            false,
            p,
        ))
        .child(btn(
            "win-max",
            if maximized {
                IconName::Copy
            } else {
                IconName::Square
            },
            WindowControlArea::Max,
            false,
            p,
        ))
        .child(btn(
            "win-close",
            IconName::X,
            WindowControlArea::Close,
            true,
            p,
        ))
}

fn app_menu(m: PopupMenu, window: &mut Window, cx: &mut Context<PopupMenu>) -> PopupMenu {
    m.submenu("File", window, cx, |m, window, cx| {
        m.menu("New File", Box::new(NewFile))
            .menu("Open…", Box::new(OpenFile))
            .submenu("Open Recent", window, cx, |mut m, _, cx| {
                let list = recent::menu_list(cx);
                if list.is_empty() {
                    m = m.item(PopupMenuItem::new("No recent files").disabled(true));
                }
                for p in list {
                    let label = p.display().to_string();
                    m = m.item(PopupMenuItem::new(label).on_click(
                        move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                            if let Some(ws) = cx.try_global::<WorkspaceRef>().map(|w| w.0.clone()) {
                                let p = p.clone();
                                let _ = ws.update(cx, |w, cx| w.open_path(p, window, cx));
                            }
                        },
                    ));
                }
                m
            })
            .menu("Home", Box::new(ShowHome))
            .separator()
            .menu("Save", Box::new(Save))
            .menu("Save As…", Box::new(SaveAs))
            .separator()
            .menu("Settings…", Box::new(OpenSettings))
            .menu("Connect an AI agent…", Box::new(ConnectAgent))
            .menu("Close Tab", Box::new(CloseTab))
    })
    .submenu("Edit", window, cx, |m, _, _| {
        m.menu("Undo", Box::new(Undo))
            .menu("Redo", Box::new(Redo))
            .separator()
            .menu("Cut", Box::new(Cut))
            .menu("Copy", Box::new(Copy))
            .menu("Paste", Box::new(Paste))
            .menu("Delete", Box::new(Delete))
            .menu("Select All", Box::new(SelectAll))
    })
    .submenu("View", window, cx, |m, _, _| {
        m.menu("Show/Hide UI", Box::new(ToggleUi))
            .menu("Zoom In", Box::new(ZoomIn))
            .menu("Zoom Out", Box::new(ZoomOut))
            .menu("Zoom to 100%", Box::new(Zoom100))
    })
    .submenu("Window", window, cx, |m, _, _| {
        m.menu("Toggle Theme", Box::new(ToggleTheme))
            .separator()
            .menu("Show Previous Tab", Box::new(PrevTab))
            .menu("Show Next Tab", Box::new(NextTab))
    })
    .submenu("Help", window, cx, |m, _, _| {
        m.menu("Keyboard Shortcuts", Box::new(ShowShortcuts))
            .item(PopupMenuItem::new("About OpenPen"))
    })
    .separator()
    .menu("Quit", Box::new(Quit))
}

fn app_menu_button(p: &Palette) -> Div {
    div().mr(px(2.)).child(
        Button::new("app-menu")
            .ghost()
            .size(px(metrics::PILL))
            .rounded_full()
            .text_color(p.fg_muted)
            .icon(Icon::new(IconName::Menu).size(px(14.)))
            .dropdown_menu(app_menu),
    )
}

/// Native app menus (macOS menu bar; on Windows the ☰ popup above).
pub fn set_app_menus(cx: &mut App) {
    cx.set_menus([
        Menu::new("OpenPen").items([
            MenuItem::action("Settings…", OpenSettings),
            MenuItem::action("Connect an AI agent…", ConnectAgent),
            MenuItem::separator(),
            MenuItem::action("Quit OpenPen", Quit),
        ]),
        Menu::new("File").items([
            MenuItem::action("New File", NewFile),
            MenuItem::action("Open…", OpenFile),
            MenuItem::action("Home", ShowHome),
            MenuItem::separator(),
            MenuItem::action("Save", Save),
            MenuItem::action("Save As…", SaveAs),
            MenuItem::separator(),
            MenuItem::action("Close Tab", CloseTab),
        ]),
        Menu::new("Edit").items([
            MenuItem::action("Undo", Undo),
            MenuItem::action("Redo", Redo),
            MenuItem::separator(),
            MenuItem::action("Cut", Cut),
            MenuItem::action("Copy", Copy),
            MenuItem::action("Paste", Paste),
            MenuItem::action("Select All", SelectAll),
        ]),
        Menu::new("View").items([
            MenuItem::action("Show/Hide UI", ToggleUi),
            MenuItem::action("Zoom In", ZoomIn),
            MenuItem::action("Zoom Out", ZoomOut),
        ]),
        Menu::new("Window").items([
            MenuItem::action("Toggle Theme", ToggleTheme),
            MenuItem::action("Show Previous Tab", PrevTab),
            MenuItem::action("Show Next Tab", NextTab),
        ]),
    ]);
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.pal();
        let body: AnyElement = match self.active {
            Some(i) => self.tabs[i].editor.clone().into_any_element(),
            None => self.home.clone().into_any_element(),
        };
        div()
            .id("workspace")
            .relative()
            .size_full()
            .bg(p.canvas)
            .text_color(p.fg)
            .text_size(px(metrics::TEXT_XS))
            .key_context(EDITOR_CTX)
            .track_focus(&self.focus)
            .on_action(cx.listener(|t, _: &NewFile, w, cx| t.new_file(w, cx)))
            .on_action(cx.listener(|t, _: &CloseTab, w, cx| {
                if let Some(a) = t.active {
                    t.close(a, w, cx)
                }
            }))
            .on_action(cx.listener(|t, _: &ShowHome, w, cx| t.activate(None, w, cx)))
            .on_action(cx.listener(|t, _: &NextTab, w, cx| t.step(1, w, cx)))
            .on_action(cx.listener(|t, _: &PrevTab, w, cx| t.step(-1, w, cx)))
            .on_action(cx.listener(|t, _: &ToggleTheme, _, cx| t.toggle_theme(cx)))
            .on_action(cx.listener(|t, _: &Save, w, cx| t.save_active(false, w, cx)))
            .on_action(cx.listener(|t, _: &SaveAs, w, cx| t.save_active(true, w, cx)))
            .on_action(cx.listener(|t, _: &OpenFile, w, cx| t.open_dialog(w, cx)))
            // `.pen` files dropped anywhere on the window open in tabs.
            .on_drop(cx.listener(|t, paths: &ExternalPaths, w, cx| t.open_dropped(paths, w, cx)))
            .on_action(|_: &OpenSettings, w, cx| Self::not_yet("Settings", w, cx))
            .on_action(|_: &CommandPalette, w, cx| Self::not_yet("The command palette", w, cx))
            .on_action(|_: &ShowShortcuts, w, cx| Self::not_yet("The shortcuts dialog", w, cx))
            .on_action(|_: &ShowChat, w, cx| {
                w.push_notification("Open a document to chat with the agent about it.", cx)
            })
            .on_action(|_: &ConnectAgent, w, cx| crate::agent::ui::open_connect_dialog(w, cx))
            .on_action(|_: &FindLayers, w, cx| Self::not_yet("Find layers", w, cx))
            .on_action(cx.listener(|t, _: &Quit, w, cx| {
                if t.should_close(w, cx) {
                    cx.quit();
                }
            }))
            .child(div().absolute().inset_0().child(body))
            .child(self.render_tab_bar(window, cx))
    }
}
