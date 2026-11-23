//! Home tab (Pen's dashboard): recent documents as a grid of cards with
//! thumbnails, or a sortable list; search, New File / Open File, context
//! menu (open, show in Explorer, rename, duplicate, remove from recents,
//! move to the Recycle Bin), keyboard navigation and `.pen` drops.
//!
//! Layout and styling follow Pen's `dashboard` renderer (see docs/home.md).
//! Thumbnails come from [`crate::preview`] on low-priority threads and stream
//! in; Home never waits for them, nor for the file stats.

pub mod files;
pub mod model;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use futures::StreamExt;
use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{Icon, Sizable, WindowExt};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use crate::preview::{self, PreviewOutcome, PreviewPool};
use crate::recent::{self, RecentStore, path_key};
use crate::theme::{Palette, PaletteExt, metrics};
use crate::workspace::{WorkspaceRef, log};
use model::{DocEntry, Layout, Nav, Sort, SortField};

/// Toolbar row (28 px + 12 px margin), list header and list row heights.
type MouseHandler =
    Box<dyn Fn(&mut HomeView, &MouseDownEvent, &mut Window, &mut Context<HomeView>)>;

const TOOLBAR_H: f32 = 40.0;
const LIST_HEADER_H: f32 = 24.0;
const LIST_ROW_H: f32 = 65.0;

/// What Home asks the workspace to do.
#[derive(Clone, Debug, PartialEq)]
pub enum HomeEvent {
    NewFile,
    OpenDialog,
    Open(PathBuf),
}

/// A thumbnail and the document mtime it was rendered from.
struct Thumb {
    image: Option<Arc<RenderImage>>,
    stamp: Option<SystemTime>,
}

struct PreviewMsg {
    doc: PathBuf,
    stamp: Option<SystemTime>,
    image: Option<Arc<RenderImage>>,
    rendered: bool,
    ms: f64,
    error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MenuCmd {
    Open,
    Reveal,
    Rename,
    Duplicate,
    RemoveFromRecents,
    Delete,
}

pub struct HomeView {
    entries: Vec<DocEntry>,
    /// Bumped on every refresh; stale stat results are dropped.
    generation: u64,
    stats_done: bool,
    thumbs: HashMap<String, Thumb>,
    pool: Option<PreviewPool>,
    /// Selected documents (path keys) and the keyboard cursor.
    selection: HashSet<String>,
    anchor: Option<PathBuf>,
    search: Entity<InputState>,
    query: String,
    layout: Layout,
    sort: Sort,
    menu: Option<Point<Pixels>>,
    focus: FocusHandle,
    scroll: ScrollHandle,
    /// Columns, content width and window height of the last frame
    /// (keyboard navigation and scrolling).
    cols: usize,
    content_width: f32,
    viewport_h: f32,
    created: Instant,
    first_frame_logged: bool,
    preview_batch: Option<(Instant, usize)>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<HomeEvent> for HomeView {}

impl HomeView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        let subs = vec![
            cx.subscribe_in(&search, window, |this, input, ev: &InputEvent, _, cx| {
                if matches!(ev, InputEvent::Change) {
                    this.query = input.read(cx).value().to_string();
                    cx.notify();
                }
            }),
            cx.observe_global::<RecentStore>(|this, cx| this.refresh(cx)),
        ];
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<PreviewMsg>();
        let pool = preview::cache_dir().map(|dir| {
            let tx = std::sync::Mutex::new(tx);
            PreviewPool::new(dir, preview::default_workers(), move |r| {
                let image = match r.outcome {
                    PreviewOutcome::Image(img) => render_image(img),
                    _ => None,
                };
                let msg = PreviewMsg {
                    doc: r.doc,
                    stamp: r.doc_mtime,
                    image,
                    rendered: r.rendered,
                    ms: r.elapsed.as_secs_f64() * 1000.0,
                    error: r.error,
                };
                let _ = tx.lock().map(|t| t.unbounded_send(msg));
            })
        });
        cx.spawn(async move |this, cx| {
            while let Some(msg) = rx.next().await {
                if this
                    .update(cx, |this, cx| this.preview_ready(msg, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        let layout = load_layout();
        let mut this = Self {
            entries: Vec::new(),
            generation: 0,
            stats_done: false,
            thumbs: HashMap::new(),
            pool,
            selection: HashSet::new(),
            anchor: None,
            search,
            query: String::new(),
            layout,
            sort: Sort::default(),
            menu: None,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            cols: 1,
            content_width: 1000.0,
            viewport_h: 800.0,
            created: Instant::now(),
            first_frame_logged: false,
            preview_batch: None,
            _subs: subs,
        };
        this.refresh(cx);
        this
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    /// Re-reads the recents, stats the files off the UI thread and queues
    /// the previews that are missing or stale. Called when Home is shown and
    /// whenever the recents change.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let store = recent::store(cx);
        let old: HashMap<String, DocEntry> = self
            .entries
            .drain(..)
            .map(|e| (path_key(&e.path), e))
            .collect();
        self.entries = model::entries_from(&store)
            .into_iter()
            .map(|mut e| {
                if let Some(o) = old.get(&path_key(&e.path)) {
                    e.modified = o.modified;
                    e.created = o.created;
                    e.missing = o.missing;
                }
                e
            })
            .collect();
        let keys: HashSet<String> = self.entries.iter().map(|e| path_key(&e.path)).collect();
        self.selection.retain(|k| keys.contains(k));
        self.thumbs.retain(|k, _| keys.contains(k));
        self.generation += 1;
        let generation = self.generation;
        let mut entries = self.entries.clone();
        let t0 = Instant::now();
        let task = cx.background_spawn(async move {
            for e in &mut entries {
                e.stat();
            }
            entries
        });
        cx.spawn(async move |this, cx| {
            let entries = task.await;
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                log(&format!(
                    "home: stat of {} recents in {:.1?}",
                    entries.len(),
                    t0.elapsed()
                ));
                this.entries = entries;
                this.stats_done = true;
                this.queue_previews();
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Asks the pool for every existing document whose thumbnail is absent
    /// or older than the file, in display order.
    fn queue_previews(&mut self) {
        let Some(pool) = &self.pool else {
            return;
        };
        let order = model::visible(&self.entries, "", Sort::default());
        let docs: Vec<PathBuf> = order
            .into_iter()
            .map(|i| &self.entries[i])
            .filter(|e| !e.missing)
            .filter(|e| {
                let stamp = e
                    .modified
                    .map(|m| SystemTime::UNIX_EPOCH + std::time::Duration::from_millis(m));
                match (self.thumbs.get(&path_key(&e.path)), stamp) {
                    (Some(t), Some(s)) => !t.stamp.is_some_and(|ts| preview::is_fresh(s, ts)),
                    _ => true,
                }
            })
            .map(|e| e.path.clone())
            .collect();
        if !docs.is_empty() {
            self.preview_batch = Some((Instant::now(), docs.len()));
        }
        pool.request(docs);
    }

    fn preview_ready(&mut self, msg: PreviewMsg, cx: &mut Context<Self>) {
        let name = msg
            .doc
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        log(&format!(
            "home: preview {name} {} in {:.1} ms{}",
            if msg.rendered {
                "rendered"
            } else {
                "from cache"
            },
            msg.ms,
            msg.error
                .as_deref()
                .map(|e| format!(" ({e})"))
                .unwrap_or_default()
        ));
        let key = path_key(&msg.doc);
        if !self.entries.iter().any(|e| path_key(&e.path) == key) {
            return;
        }
        let old = self.thumbs.insert(
            key,
            Thumb {
                image: msg.image,
                stamp: msg.stamp,
            },
        );
        if let Some(img) = old.and_then(|t| t.image) {
            cx.drop_image(img, None);
        }
        if let Some((t0, n)) = self.preview_batch
            && self.pool.as_ref().is_some_and(|p| p.pending() == 0)
        {
            log(&format!("home: {n} previews ready in {:.1?}", t0.elapsed()));
            self.preview_batch = None;
        }
        cx.notify();
    }

    // ------------------------------------------------------------ model

    fn visible(&self) -> Vec<usize> {
        let sort = match self.layout {
            Layout::Grid => Sort::default(),
            Layout::List => self.sort,
        };
        model::visible(&self.entries, &self.query, sort)
    }

    fn entry(&self, path: &Path) -> Option<&DocEntry> {
        let k = path_key(path);
        self.entries.iter().find(|e| path_key(&e.path) == k)
    }

    fn is_selected(&self, path: &Path) -> bool {
        self.selection.contains(&path_key(path))
    }

    fn selected_paths(&self) -> Vec<PathBuf> {
        self.visible()
            .into_iter()
            .map(|i| &self.entries[i].path)
            .filter(|p| self.is_selected(p))
            .cloned()
            .collect()
    }

    fn select_only(&mut self, path: &Path) {
        self.selection.clear();
        self.selection.insert(path_key(path));
        self.anchor = Some(path.to_path_buf());
    }

    fn click_select(&mut self, path: &Path, m: Modifiers) {
        if m.control || m.platform {
            let k = path_key(path);
            if !self.selection.remove(&k) {
                self.selection.insert(k);
            }
            self.anchor = Some(path.to_path_buf());
        } else if m.shift
            && let Some(a) = self.anchor.clone()
        {
            let vis = self.visible();
            let pos = |p: &Path| {
                let k = path_key(p);
                vis.iter()
                    .position(|&i| path_key(&self.entries[i].path) == k)
            };
            if let (Some(x), Some(y)) = (pos(&a), pos(path)) {
                let (lo, hi) = (x.min(y), x.max(y));
                self.selection = vis[lo..=hi]
                    .iter()
                    .map(|&i| path_key(&self.entries[i].path))
                    .collect();
            } else {
                self.select_only(path);
            }
        } else {
            self.select_only(path);
        }
    }

    fn navigate(&mut self, nav: Nav, cx: &mut Context<Self>) {
        let vis = self.visible();
        let cur = self.anchor.as_ref().and_then(|a| {
            let k = path_key(a);
            vis.iter()
                .position(|&i| path_key(&self.entries[i].path) == k)
        });
        let cols = match self.layout {
            Layout::Grid => self.cols,
            Layout::List => 1,
        };
        if let Some(n) = model::navigate(cur, vis.len(), cols, nav) {
            let p = self.entries[vis[n]].path.clone();
            self.select_only(&p);
            self.scroll_to(n, cols);
            cx.notify();
        }
    }

    /// Keeps the card at visible position `n` in view.
    fn scroll_to(&self, n: usize, cols: usize) {
        let row = (n / cols.max(1)) as f32;
        let top0 = metrics::TAB_BAR_HEIGHT + 10.0 + TOOLBAR_H;
        let (top, h) = match self.layout {
            Layout::Grid => {
                let card_w = model::grid_columns(self.content_width).1;
                let h = (card_w * 10.0 / 16.0).round() + 62.0;
                (top0 + row * (h + model::GRID_GAP), h)
            }
            Layout::List => (top0 + LIST_HEADER_H + row * LIST_ROW_H, LIST_ROW_H),
        };
        let off = self.scroll.offset();
        let y = f32::from(off.y);
        // Visible band: below the tab bar, above the bottom padding.
        let (lo, hi) = (metrics::TAB_BAR_HEIGHT + 8.0, self.viewport_h - 24.0);
        let new_y = if top + y < lo {
            lo - top
        } else if top + h + y > hi {
            hi - top - h
        } else {
            return;
        };
        self.scroll.set_offset(point(off.x, px(new_y.min(0.0))));
    }

    // --------------------------------------------------------- commands

    fn open(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        if self.entry(&path).is_some_and(|e| e.missing) || !path.is_file() {
            self.offer_remove_missing(path, window, cx);
            return;
        }
        cx.emit(HomeEvent::Open(path));
    }

    fn offer_remove_missing(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let name = file_name(&path);
        let rx = window.prompt(
            PromptLevel::Warning,
            &format!("“{name}” can't be found"),
            Some(&format!(
                "It may have been moved, renamed or deleted.\n{}",
                path.display()
            )),
            &["Remove from Recents", "Cancel"],
            cx,
        );
        cx.spawn(async move |_, cx| {
            if rx.await == Ok(0) {
                cx.update(|cx| remove_from_recents(&[path], cx));
            }
        })
        .detach();
    }

    fn run(&mut self, cmd: MenuCmd, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        let paths = self.selected_paths();
        let Some(first) = paths.first().cloned() else {
            cx.notify();
            return;
        };
        match cmd {
            MenuCmd::Open => self.open(first, window, cx),
            MenuCmd::Reveal => {
                if let Err(e) = files::reveal(&first) {
                    window.push_notification(format!("Could not open the folder: {e}"), cx);
                }
            }
            MenuCmd::Rename => self.rename_dialog(first, window, cx),
            MenuCmd::Duplicate => match files::duplicate(&first) {
                Ok(copy) => {
                    let key = path_key(&copy);
                    recent::update(cx, |s| s.push(copy.clone(), recent::now_ms()));
                    self.selection = HashSet::from([key]);
                    self.anchor = Some(copy);
                }
                Err(e) => window.push_notification(format!("Could not duplicate: {e}"), cx),
            },
            MenuCmd::RemoveFromRecents => remove_from_recents(&paths, cx),
            MenuCmd::Delete => self.confirm_delete(paths, window, cx),
        }
        cx.notify();
    }

    fn open_in_tab(path: &Path, cx: &App) -> bool {
        cx.try_global::<WorkspaceRef>()
            .and_then(|w| w.0.upgrade())
            .is_some_and(|w| w.read(cx).doc_for_path(path, cx).is_some())
    }

    fn rename_dialog(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if Self::open_in_tab(&path, cx) {
            window.push_notification(
                format!("Close “{}” before renaming it.", file_name(&path)),
                cx,
            );
            return;
        }
        let name = file_name(&path);
        let stem_len = name.len()
            - if name.to_lowercase().ends_with(".pen") {
                4
            } else {
                0
            };
        let input = cx.new(|cx| InputState::new(window, cx).default_value(name.clone()));
        let home = cx.entity().downgrade();
        let submit = {
            let input = input.clone();
            let home = home.clone();
            let path = path.clone();
            Rc::new(move |window: &mut Window, cx: &mut App| {
                let value = input.read(cx).value().to_string();
                let Some(new_name) = model::valid_pen_name(&value) else {
                    window.push_notification("That is not a valid file name.", cx);
                    return;
                };
                window.close_dialog(cx);
                if new_name == file_name(&path) {
                    return;
                }
                match files::rename(&path, &new_name) {
                    Ok(target) => {
                        if let Some(dir) = preview::cache_dir() {
                            preview::rename_cached(&dir, &path, &target);
                        }
                        let _ = home.update(cx, |h, _| {
                            if let Some(t) = h.thumbs.remove(&path_key(&path)) {
                                h.thumbs.insert(path_key(&target), t);
                            }
                            h.select_only(&target);
                        });
                        recent::update(cx, |s| {
                            s.rename(&path, target.clone());
                        });
                    }
                    Err(e) => window.push_notification(format!("Could not rename: {e}"), cx),
                }
            })
        };
        let sub = {
            let submit = submit.clone();
            cx.subscribe_in(&input, window, move |_, _, ev: &InputEvent, window, cx| {
                if matches!(ev, InputEvent::PressEnter { .. }) {
                    submit(window, cx);
                }
            })
        };
        // Like Pen: focus the field with the name selected, without the
        // extension (after the dialog mounted, so its focus does not win).
        let field = input.clone();
        window.defer(cx, move |window, cx| {
            field.update(cx, |s, cx| {
                s.focus(window, cx);
                s.set_selected_range(0..stem_len, cx);
            });
        });
        window.open_dialog(cx, move |d, _, cx| {
            let p = cx.pal();
            let submit = submit.clone();
            let _keep = &sub;
            d.title(div().text_size(px(16.)).child("Rename"))
                .width(px(400.))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(12.))
                        .child(
                            div()
                                .text_size(px(metrics::TEXT_XS))
                                .text_color(p.fg_muted)
                                .child("This also renames the file on your disk."),
                        )
                        .child(Input::new(&input))
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap(px(8.))
                                .child(
                                    dialog_button("rename-cancel", "Cancel", false, &p)
                                        .on_click(|_, window, cx| window.close_dialog(cx)),
                                )
                                .child(
                                    dialog_button("rename-save", "Save", true, &p)
                                        .on_click(move |_, window, cx| submit(window, cx)),
                                ),
                        ),
                )
        });
    }

    fn confirm_delete(&mut self, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        let paths: Vec<PathBuf> = paths
            .into_iter()
            .filter(|p| !self.entry(p).is_some_and(|e| e.missing))
            .collect();
        if paths.is_empty() {
            return;
        }
        if let Some(p) = paths.iter().find(|p| Self::open_in_tab(p, cx)) {
            window.push_notification(format!("Close “{}” before deleting it.", file_name(p)), cx);
            return;
        }
        let title = if paths.len() == 1 {
            format!("Move “{}” to the Recycle Bin?", file_name(&paths[0]))
        } else {
            format!("Move {} documents to the Recycle Bin?", paths.len())
        };
        let rx = window.prompt(
            PromptLevel::Warning,
            &title,
            Some("You can restore them from the Recycle Bin."),
            &["Move to Recycle Bin", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |_, cx| {
            if rx.await != Ok(0) {
                return;
            }
            let _ = cx.update(|window, cx| {
                let mut gone = Vec::new();
                for p in &paths {
                    match files::move_to_trash(p) {
                        Ok(()) => gone.push(p.clone()),
                        Err(e) => window.push_notification(
                            format!("Could not delete {}: {e}", file_name(p)),
                            cx,
                        ),
                    }
                }
                if let Some(dir) = preview::cache_dir() {
                    for p in &gone {
                        preview::remove_cached(&dir, p);
                    }
                }
                remove_from_recents(&gone, cx);
            });
        })
        .detach();
    }

    fn set_layout(&mut self, layout: Layout, cx: &mut Context<Self>) {
        self.layout = layout;
        save_layout(layout);
        cx.notify();
    }

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // Only when the Home itself has focus (not the search field).
        if !self.focus.is_focused(window) {
            if ev.keystroke.key == "escape" && self.menu.is_some() {
                self.menu = None;
                cx.notify();
            }
            return;
        }
        let k = &ev.keystroke;
        let m = k.modifiers;
        let nav = match k.key.as_str() {
            "left" => Some(Nav::Left),
            "right" => Some(Nav::Right),
            "up" => Some(Nav::Up),
            "down" => Some(Nav::Down),
            "home" => Some(Nav::Home),
            "end" => Some(Nav::End),
            _ => None,
        };
        if let Some(nav) = nav {
            self.menu = None;
            self.navigate(nav, cx);
            cx.stop_propagation();
            return;
        }
        match k.key.as_str() {
            "enter" if !m.modified() => {
                let sel = self.selected_paths();
                if sel.len() == 1 {
                    self.open(sel[0].clone(), window, cx);
                }
            }
            "escape" => {
                if self.menu.take().is_none() {
                    self.selection.clear();
                }
                cx.notify();
            }
            "a" if (m.control || m.platform) => {
                self.selection = self
                    .visible()
                    .into_iter()
                    .map(|i| path_key(&self.entries[i].path))
                    .collect();
                cx.notify();
            }
            "f" if (m.control || m.platform) => {
                self.search.update(cx, |s, cx| s.focus(window, cx));
            }
            "delete" if !m.modified() => {
                self.run(MenuCmd::RemoveFromRecents, window, cx);
            }
            "f2" => {
                if let [p] = self.selected_paths().as_slice() {
                    self.rename_dialog(p.clone(), window, cx);
                }
            }
            "menu" => {
                if !self.selection.is_empty() {
                    let c = window.viewport_size();
                    self.menu = Some(point(c.width / 2.0, c.height / 2.0));
                    cx.notify();
                }
            }
            _ => return,
        }
        cx.stop_propagation();
    }

    // ------------------------------------------------------------ render

    fn colors(p: &Palette) -> HomeColors {
        if p.dark {
            HomeColors {
                background: rgb(0x18181b).into(),
                muted: ca(0x27272a, 0.7),
                card: rgb(0x141414).into(),
                border: rgb(0x202020).into(),
                faint: ca(0xa1a1aa, 0.7),
                placeholder: rgb(0x404040).into(),
                selected: rgb(0x60a5fa).into(),
                selected_bg: ca(0x60a5fa, 0.15),
            }
        } else {
            HomeColors {
                background: rgb(0xfafafa).into(),
                muted: ca(0xf4f4f5, 0.7),
                card: rgb(0xffffff).into(),
                border: rgb(0xe4e4e7).into(),
                faint: ca(0x71717a, 0.7),
                placeholder: rgb(0xd0d0d0).into(),
                selected: rgb(0x3b82f6).into(),
                selected_bg: ca(0x3b82f6, 0.15),
            }
        }
    }

    fn render_toolbar(&self, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let layout_btn = |id: &'static str, icon: IconName, l: Layout, cx: &mut Context<Self>| {
            let active = self.layout == l;
            div()
                .id(id)
                .size(px(24.))
                .rounded(px(6.))
                .flex()
                .items_center()
                .justify_center()
                .when(active, |d| d.bg(p.segment_active).shadow_sm())
                .text_color(if active { p.fg } else { p.fg_muted })
                .child(Icon::new(icon).size(px(14.)))
                .on_click(cx.listener(move |this, _, _, cx| this.set_layout(l, cx)))
        };
        div()
            .mb(px(12.))
            .flex()
            .items_center()
            .justify_between()
            // Keep the search field's focus and the selection.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .gap(px(8.))
            .child(
                div().flex().items_center().gap(px(4.)).child(
                    div()
                        .h(px(28.))
                        .px(px(12.))
                        .flex()
                        .items_center()
                        .rounded_full()
                        .bg(p.tab_active)
                        .text_size(px(metrics::TEXT_XS))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(p.fg)
                        .child("Recents"),
                ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div().w(px(200.)).child(
                            Input::new(&self.search).small().cleanable(true).prefix(
                                Icon::new(IconName::Search)
                                    .size(px(14.))
                                    .text_color(p.fg_muted),
                            ),
                        ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(2.))
                            .rounded(px(8.))
                            .bg(p.hover)
                            .p(px(2.))
                            .child(layout_btn(
                                "home-grid",
                                IconName::LayoutGrid,
                                Layout::Grid,
                                cx,
                            ))
                            .child(layout_btn("home-list", IconName::List, Layout::List, cx)),
                    )
                    .child(
                        div()
                            .id("home-open")
                            .h(px(28.))
                            .px(px(10.))
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .rounded(px(metrics::RADIUS_SM))
                            .text_size(px(metrics::TEXT_XS))
                            .text_color(p.fg_muted)
                            .hover(|s| s.bg(p.hover).text_color(p.fg))
                            .child(Icon::new(IconName::FolderOpen).size(px(14.)))
                            .child("Open File")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(HomeEvent::OpenDialog))),
                    )
                    .child(
                        div()
                            .id("home-new")
                            .h(px(28.))
                            .px(px(10.))
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .rounded(px(metrics::RADIUS_SM))
                            .text_size(px(metrics::TEXT_XS))
                            .font_weight(FontWeight::MEDIUM)
                            .bg(if p.dark { rgb(0xffffff) } else { rgb(0x020202) })
                            .text_color(if p.dark { rgb(0x000000) } else { rgb(0xfafafa) })
                            .hover(|s| s.opacity(0.85))
                            .child(Icon::new(IconName::Plus).size(px(14.)))
                            .child("New File")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(HomeEvent::NewFile))),
                    ),
            )
    }

    fn thumb(&self, path: &Path) -> Option<&Arc<RenderImage>> {
        self.thumbs
            .get(&path_key(path))
            .and_then(|t| t.image.as_ref())
    }

    fn card_mouse(&self, path: PathBuf) -> (MouseHandler, MouseHandler) {
        let p2 = path.clone();
        (
            Box::new(
                move |this: &mut Self,
                      ev: &MouseDownEvent,
                      window: &mut Window,
                      cx: &mut Context<Self>| {
                    cx.stop_propagation();
                    window.focus(&this.focus, cx);
                    this.menu = None;
                    if ev.click_count >= 2 {
                        this.select_only(&path);
                        this.open(path.clone(), window, cx);
                    } else {
                        this.click_select(&path, ev.modifiers);
                    }
                    cx.notify();
                },
            ),
            Box::new(
                move |this: &mut Self,
                      ev: &MouseDownEvent,
                      window: &mut Window,
                      cx: &mut Context<Self>| {
                    cx.stop_propagation();
                    window.focus(&this.focus, cx);
                    if !this.is_selected(&p2) {
                        this.select_only(&p2);
                    }
                    this.menu = Some(ev.position);
                    cx.notify();
                },
            ),
        )
    }

    fn more_button(
        &self,
        ix: usize,
        selected: bool,
        group: SharedString,
        p: &Palette,
        path: PathBuf,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(("home-more", ix))
            .flex_none()
            .size(px(24.))
            .rounded(px(6.))
            .flex()
            .items_center()
            .justify_center()
            .bg(p.hover)
            .text_color(p.fg)
            .when(!selected, |d| {
                d.opacity(0.0).group_hover(group, |s| s.opacity(1.0))
            })
            .hover(|s| s.bg(p.control_hover))
            .child(Icon::new(IconName::Ellipsis).size(px(14.)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    window.focus(&this.focus, cx);
                    if !this.is_selected(&path) {
                        this.select_only(&path);
                    }
                    this.menu = Some(ev.position);
                    cx.notify();
                }),
            )
    }

    #[allow(clippy::too_many_arguments)]
    fn render_card(
        &self,
        ix: usize,
        e: &DocEntry,
        width: f32,
        now: u64,
        c: &HomeColors,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = self.is_selected(&e.path);
        let group: SharedString = format!("home-card-{ix}").into();
        let thumb = self.thumb(&e.path).cloned();
        let preview_h = (width * 10.0 / 16.0).round();
        let (down, right) = self.card_mouse(e.path.clone());
        let placeholder = c.placeholder;
        let preview = div()
            .w_full()
            .h(px(preview_h))
            .flex()
            .items_center()
            .justify_center()
            .overflow_hidden()
            .bg(c.muted)
            .map(|d| match thumb {
                Some(img) => d
                    .p(px(16.))
                    .child(img_from(img).size_full().object_fit(ObjectFit::Contain)),
                None if e.missing => d.child(
                    Icon::new(IconName::FileX)
                        .size(px(28.))
                        .text_color(placeholder),
                ),
                None => d.child(pen_placeholder(placeholder)),
            });
        let title = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .text_size(px(12.))
            .line_height(px(16.))
            .child(
                div().flex_none().text_color(c.faint).child(
                    Icon::new(if e.missing {
                        IconName::TriangleAlert
                    } else {
                        IconName::File
                    })
                    .size(px(12.)),
                ),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .when_some(e.folder.clone(), |d, f| {
                        d.child(div().text_color(c.faint).child(format!("{f} / ")))
                    })
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.fg)
                            .child(e.name.clone()),
                    )
                    .flex()
                    .overflow_hidden(),
            );
        let subtitle = div()
            .mt(px(4.))
            .truncate()
            .text_size(px(11.))
            .line_height(px(14.))
            .text_color(if e.missing { p.amber } else { c.faint })
            .child(if self.stats_done || e.modified.is_some() || e.missing {
                e.subtitle(now)
            } else {
                format!("Opened {}", model::relative_time(now, e.opened_at))
            });
        div()
            .id(("home-card", ix))
            .group(group.clone())
            .relative()
            .w(px(width))
            .flex()
            .flex_col()
            .overflow_hidden()
            .rounded(px(12.))
            .border_1()
            .border_color(if selected { c.selected } else { c.border })
            .bg(c.background)
            .when(e.missing, |d| d.opacity(0.75))
            .on_mouse_down(MouseButton::Left, cx.listener(down))
            .on_mouse_down(MouseButton::Right, cx.listener(right))
            .child(preview)
            .child(
                div()
                    .h(px(60.))
                    .flex()
                    .items_start()
                    .gap(px(12.))
                    .border_t_1()
                    .border_color(c.border)
                    .bg(c.card)
                    .p(px(12.))
                    .child(div().min_w_0().flex_1().child(title).child(subtitle))
                    .child(self.more_button(ix, selected, group, p, e.path.clone(), cx)),
            )
            .when(selected, |d| {
                // Pen: border-blue-500 + ring-1 (2 px total).
                d.child(
                    div()
                        .absolute()
                        .inset_0()
                        .rounded(px(12.))
                        .border_1()
                        .border_color(c.selected),
                )
            })
    }

    fn render_grid(
        &self,
        vis: &[usize],
        width: f32,
        now: u64,
        c: &HomeColors,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (cols, card_w) = model::grid_columns(width);
        let mut grid = div().flex().flex_col().gap(px(model::GRID_GAP));
        for (r, row) in vis.chunks(cols).enumerate() {
            let mut line = div().id(("home-row", r)).flex().gap(px(model::GRID_GAP));
            for &i in row {
                line = line.child(self.render_card(i, &self.entries[i], card_w, now, c, p, cx));
            }
            grid = grid.child(line);
        }
        grid.into_any_element()
    }

    fn render_list(
        &self,
        vis: &[usize],
        now: u64,
        c: &HomeColors,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let cols = |d: Div| d.flex().items_center().gap(px(16.));
        let header_cell =
            |label: &'static str, field: SortField, w: Option<f32>, cx: &mut Context<Self>| {
                let active = self.sort.field == field;
                div()
                    .id(label)
                    .map(|d| match w {
                        Some(w) => d.w(px(w)).flex_none(),
                        None => d.flex_1().min_w_0(),
                    })
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .text_color(if active { p.fg } else { c.faint })
                    .child(label)
                    .when(active, |d| {
                        d.child(
                            Icon::new(if self.sort.descending {
                                IconName::ArrowDown
                            } else {
                                IconName::ArrowUp
                            })
                            .size(px(12.)),
                        )
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.sort = this.sort.toggled(field);
                        cx.notify();
                    }))
            };
        let mut list = div().flex().flex_col().child(
            cols(div())
                .px(px(12.))
                .pb(px(8.))
                .border_b_1()
                .border_color(c.border)
                .text_size(px(10.))
                .font_weight(FontWeight::SEMIBOLD)
                .child(header_cell("NAME", SortField::Name, None, cx))
                .child(header_cell(
                    "LAST USED",
                    SortField::LastUsed,
                    Some(140.),
                    cx,
                ))
                .child(header_cell("CREATED", SortField::Created, Some(140.), cx))
                .child(div().w(px(24.)).flex_none()),
        );
        for &i in vis {
            let e = &self.entries[i];
            let selected = self.is_selected(&e.path);
            let group: SharedString = format!("home-row-{i}").into();
            let (down, right) = self.card_mouse(e.path.clone());
            let thumb = self.thumb(&e.path).cloned();
            list = list.child(
                cols(div())
                    .id(("home-list-row", i))
                    .group(group.clone())
                    .px(px(12.))
                    .py(px(10.))
                    .border_b_1()
                    .border_color(c.border)
                    .text_size(px(13.))
                    .when(selected, |d| d.bg(c.selected_bg))
                    .when(e.missing, |d| d.opacity(0.75))
                    .on_mouse_down(MouseButton::Left, cx.listener(down))
                    .on_mouse_down(MouseButton::Right, cx.listener(right))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap(px(12.))
                            .child(
                                div()
                                    .flex_none()
                                    .w(px(64.))
                                    .h(px(44.))
                                    .rounded(px(4.))
                                    .overflow_hidden()
                                    .bg(c.muted)
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .map(|d| match thumb {
                                        Some(img) => d.child(
                                            img_from(img)
                                                .size_full()
                                                .object_fit(ObjectFit::Contain),
                                        ),
                                        None => d.child(
                                            Icon::new(if e.missing {
                                                IconName::FileX
                                            } else {
                                                IconName::File
                                            })
                                            .size(px(14.))
                                            .text_color(c.faint),
                                        ),
                                    }),
                            )
                            .child(
                                div().flex_none().text_color(c.faint).child(
                                    Icon::new(if e.missing {
                                        IconName::TriangleAlert
                                    } else {
                                        IconName::File
                                    })
                                    .size(px(12.)),
                                ),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .flex()
                                    .overflow_hidden()
                                    .truncate()
                                    .when_some(e.folder.clone(), |d, f| {
                                        d.child(div().text_color(c.faint).child(format!("{f} / ")))
                                    })
                                    .child(
                                        div()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(p.fg)
                                            .child(e.name.clone()),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .w(px(140.))
                            .flex_none()
                            .truncate()
                            .text_size(px(12.))
                            .text_color(if e.missing { p.amber } else { c.faint })
                            .child(if e.missing {
                                "File not found".to_string()
                            } else {
                                e.subtitle(now)
                            }),
                    )
                    .child(
                        div()
                            .w(px(140.))
                            .flex_none()
                            .truncate()
                            .text_size(px(12.))
                            .text_color(c.faint)
                            .child(
                                e.created
                                    .map(model::short_date)
                                    .unwrap_or_else(|| "—".into()),
                            ),
                    )
                    .child(self.more_button(10_000 + i, selected, group, p, e.path.clone(), cx)),
            );
        }
        list.into_any_element()
    }

    fn render_menu(
        &self,
        at: Point<Pixels>,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let sel = self.selected_paths();
        let n = sel.len();
        let single = (n == 1).then(|| sel[0].clone());
        let missing = single
            .as_ref()
            .is_some_and(|s| self.entry(s).is_some_and(|e| e.missing));
        let existing = sel
            .iter()
            .filter(|s| !self.entry(s).is_some_and(|e| e.missing))
            .count();
        let mut rows: Vec<Option<(MenuCmd, String, bool, bool)>> = Vec::new();
        if single.is_some() {
            rows.push(Some((MenuCmd::Open, "Open".into(), !missing, false)));
            rows.push(Some((
                MenuCmd::Reveal,
                files::reveal_label().into(),
                !missing,
                false,
            )));
            rows.push(Some((MenuCmd::Rename, "Rename…".into(), !missing, false)));
            rows.push(Some((
                MenuCmd::Duplicate,
                "Duplicate".into(),
                !missing,
                false,
            )));
            rows.push(None);
        }
        rows.push(Some((
            MenuCmd::RemoveFromRecents,
            if n == 1 {
                "Remove from Recents".into()
            } else {
                format!("Remove {n} from Recents")
            },
            n > 0,
            false,
        )));
        if existing > 0 {
            rows.push(Some((
                MenuCmd::Delete,
                if existing == 1 {
                    "Move to Recycle Bin".into()
                } else {
                    format!("Move {existing} to Recycle Bin")
                },
                true,
                true,
            )));
        }
        let accent: Hsla = rgb(0x3b82f6).into();
        let destructive: Hsla = if p.dark {
            rgb(0xf87171).into()
        } else {
            rgb(0xef4444).into()
        };
        let mut list = div()
            .id("home-menu")
            .occlude()
            .min_w(px(180.))
            .p(px(4.))
            .flex()
            .flex_col()
            .rounded(px(metrics::RADIUS_MD))
            .bg(p.popover)
            .border_1()
            .border_color(if p.dark {
                hsla(0., 0., 1., 0.1)
            } else {
                hsla(0., 0., 0., 0.05)
            })
            .shadow_lg()
            .text_size(px(metrics::TEXT_XS))
            .line_height(px(16.))
            .text_color(if p.dark { rgb(0xf4f4f5) } else { rgb(0x18181b) })
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.menu = None;
                cx.notify();
            }));
        for (i, row) in rows.into_iter().enumerate() {
            let Some((cmd, label, enabled, danger)) = row else {
                list = list.child(div().h_px().mx(px(-4.)).my(px(4.)).bg(p.border));
                continue;
            };
            list = list.child(
                div()
                    .id(("home-menu-item", i))
                    .px(px(8.))
                    .py(px(6.))
                    .rounded(px(metrics::RADIUS_SM))
                    .when(danger, |d| d.text_color(destructive))
                    .when(!enabled, |d| d.opacity(0.5))
                    .when(enabled, |d| {
                        d.hover(|s| {
                            s.bg(if danger { destructive } else { accent })
                                .text_color(rgb(0xffffff))
                        })
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.run(cmd, window, cx)
                            },
                        ))
                    })
                    .child(label),
            );
        }
        deferred(
            anchored()
                .position(at)
                .snap_to_window_with_margin(px(8.))
                .child(list),
        )
        .with_priority(2)
    }
}

struct HomeColors {
    /// `--background` (card body).
    background: Hsla,
    /// `--muted` / 70 (preview well).
    muted: Hsla,
    /// `--card` (card footer).
    card: Hsla,
    border: Hsla,
    /// `--muted-foreground` / 70.
    faint: Hsla,
    /// Empty-preview glyph.
    placeholder: Hsla,
    selected: Hsla,
    selected_bg: Hsla,
}

fn ca(hex: u32, a: f32) -> Hsla {
    let mut c: Hsla = rgb(hex).into();
    c.a = a;
    c
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

fn img_from(image: Arc<RenderImage>) -> Img {
    img(ImageSource::Render(image))
}

/// Straight RGBA preview pixels → GPUI image (BGRA frames).
fn render_image(img: preview::PreviewImage) -> Option<Arc<RenderImage>> {
    let mut data = img.rgba;
    for px in data.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
    }
    let buf = image::RgbaImage::from_raw(img.width, img.height, data)?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buf)])))
}

/// Pen's empty-preview glyph: a 36x46 pen-nib outline.
fn pen_placeholder(color: Hsla) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let o = bounds.origin;
            let pt = |x: f32, y: f32| point(o.x + px(x), o.y + px(y));
            let mut b = PathBuilder::stroke(px(1.));
            b.move_to(pt(35.15, 0.38));
            b.line_to(pt(35.15, 26.85));
            b.line_to(pt(17.79, 45.33));
            b.line_to(pt(0.38, 26.85));
            b.line_to(pt(0.38, 0.38));
            b.close();
            if let Ok(path) = b.build() {
                window.paint_path(path, color);
            }
        },
    )
    .w(px(36.))
    .h(px(46.))
}

/// Removes documents from the recents (their cached PNGs stay).
pub fn remove_from_recents(paths: &[PathBuf], cx: &mut App) {
    if paths.is_empty() {
        return;
    }
    recent::update(cx, |s| {
        s.remove(paths);
    });
}

fn layout_store() -> Option<PathBuf> {
    recent::data_dir().map(|d| d.join("home-layout.txt"))
}

fn load_layout() -> Layout {
    match layout_store()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .as_deref()
        .map(str::trim)
    {
        Some("list") => Layout::List,
        _ => Layout::Grid,
    }
}

fn save_layout(l: Layout) {
    if let Some(p) = layout_store() {
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(p, if l == Layout::List { "list" } else { "grid" });
    }
}

fn dialog_button(
    id: &'static str,
    label: &'static str,
    primary: bool,
    p: &Palette,
) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(32.))
        .px(px(12.))
        .flex()
        .items_center()
        .rounded(px(metrics::RADIUS_SM))
        .text_size(px(metrics::TEXT_XS))
        .font_weight(FontWeight::MEDIUM)
        .map(|d| {
            if primary {
                d.bg(p.primary)
                    .text_color(p.primary_fg)
                    .hover(|s| s.opacity(0.9))
            } else {
                d.border_1()
                    .border_color(p.input)
                    .text_color(p.fg)
                    .hover(|s| s.bg(p.hover))
            }
        })
        .child(label)
}

impl Focusable for HomeView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for HomeView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.first_frame_logged {
            self.first_frame_logged = true;
            log(&format!(
                "home: first frame {:.1?} after creation ({} recents)",
                self.created.elapsed(),
                self.entries.len()
            ));
        }
        let p = cx.pal();
        let c = Self::colors(&p);
        let now = recent::now_ms();
        let vis = self.visible();
        // Content box: window minus px-5 on both sides.
        let width = f32::from(window.viewport_size().width) - 40.0;
        self.cols = model::grid_columns(width).0;
        self.content_width = width;
        self.viewport_h = f32::from(window.viewport_size().height);
        let body: AnyElement = if self.entries.is_empty() || vis.is_empty() {
            let text = if self.entries.is_empty() {
                "No documents here yet".to_string()
            } else {
                format!("No documents match “{}”", self.query.trim())
            };
            div()
                .rounded(px(8.))
                .border_1()
                .border_dashed()
                .border_color(p.border)
                .py(px(80.))
                .flex()
                .flex_col()
                .items_center()
                .gap(px(12.))
                .text_size(px(14.))
                .text_color(p.fg_muted)
                .child(text)
                .when(self.entries.is_empty(), |d| {
                    d.child(div().text_size(px(metrics::TEXT_XS)).child(
                        "Create a file or open one from disk. You can also drop a .pen file here.",
                    ))
                })
                .into_any_element()
        } else {
            match self.layout {
                Layout::Grid => self.render_grid(&vis, width, now, &c, &p, cx),
                Layout::List => self.render_list(&vis, now, &c, &p, cx),
            }
        };
        let dragging = cx.has_active_drag();
        div()
            .id("home")
            .key_context("Home")
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .bg(p.canvas)
            .on_key_down(cx.listener(Self::on_key))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    window.focus(&this.focus, cx);
                    this.menu = None;
                    this.selection.clear();
                    cx.notify();
                }),
            )
            .child(
                div()
                    .id("home-scroll")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .pt(px(metrics::TAB_BAR_HEIGHT + 10.))
                    .px(px(20.))
                    .pb(px(24.))
                    .child(self.render_toolbar(&p, cx))
                    .child(body),
            )
            .when_some(self.menu, |d, at| d.child(self.render_menu(at, &p, cx)))
            .when(dragging, |d| {
                d.child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .border_2()
                        .border_color(p.primary.opacity(0.5))
                        .bg(p.fg.opacity(0.5))
                        .child(
                            div()
                                .rounded(px(8.))
                                .bg(c.background.opacity(0.9))
                                .px(px(16.))
                                .py(px(8.))
                                .text_size(px(14.))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(p.fg)
                                .shadow_sm()
                                .child("Drop .pen file to open"),
                        ),
                )
            })
    }
}

use std::rc::Rc;
