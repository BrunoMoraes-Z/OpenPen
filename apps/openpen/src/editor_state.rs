//! Per-document editor state shared by the panels and the canvas (GPUI
//! model entity). Owns the [`SceneDocument`]; every mutation ends with
//! [`EditorState::changed`] (incremental pipeline update + panel refresh).

use std::collections::HashSet;
use std::path::PathBuf;

use gpui_kit::*;

use crate::agent::fx::AgentFx;
use crate::agent::{AgentInfo, AppScriptHost, touched_nodes};
use crate::inspector::fill::{FillEditTarget, PaintOp};
use crate::inspector::model::{PropEdit, PropertySource};
use crate::layers::model::{FlatRow, LayerId, LayerTreeSource, containers_below, flatten, reveal};
use crate::scene_document::{Arrange, SceneDocument, id_of, key_of};
use crate::tools::Tool;
use crate::zoom;

pub struct EditorState {
    pub doc: SceneDocument,
    /// File on disk (`None` = never saved).
    pub path: Option<PathBuf>,
    pub name: SharedString,
    /// Background load in progress (the document is an empty placeholder).
    pub loading: bool,
    pub load_error: Option<String>,
    pub selection: Vec<LayerId>,
    /// Node highlighted from the layer tree (drawn on the canvas).
    pub hover: Option<LayerId>,
    pub expanded: HashSet<LayerId>,
    pub tool: Tool,
    /// Last tool picked in each toolbar group (shown on the group button).
    pub group_choice: [Tool; 2],
    pub zoom: f32,
    /// One-shot request for the canvas to fit the content (after load).
    pub fit_requested: bool,
    /// One-shot request to zoom to the selection.
    pub fit_selection_requested: bool,
    /// Screen insets (top, right, bottom, left) covered by the floating
    /// panels; zoom-to-fit fits into the free area.
    pub view_insets: [f32; 4],
    rows: Vec<FlatRow>,
    /// Open context menu (drawn by the editor).
    pub context_menu: Option<ContextMenuReq>,
    /// Layer the layers panel should start renaming.
    pub rename_request: Option<LayerId>,
    /// Bumped when the selection changes from outside the layer tree (the
    /// tree scrolls the selection into view).
    pub reveal_tick: u64,
    /// Selection / expanded rows with their id paths, taken after the last
    /// change: structural edits can give nodes new keys, and these let
    /// [`EditorState::changed`] find them again.
    sel_snap: (Vec<LayerId>, Vec<Option<String>>),
    exp_snap: Vec<(LayerId, String)>,
    /// Flashes and cursors of agents editing this document.
    pub agent_fx: AgentFx,
    /// Paint open in the inspector's fill popover (gradient handles on the
    /// canvas; Delete removes its selected stop).
    pub fill_edit: Option<FillEditTarget>,
    /// The canvas is dragging one of its gradient handles.
    pub fill_handle_drag: bool,
    /// A task polls background image decodes (see [`Self::watch_assets`]).
    assets_watch: bool,
    /// Vector edit mode is on (the toolbar shows the path sub-tools).
    pub path_mode: bool,
    /// Path sub-tool "Cut" (X): with [`Tool::Move`], clicks cut the path.
    /// Any tool change clears it.
    pub path_cut: bool,
    /// One-shot request from the path sub-toolbar's "Done".
    pub path_done_requested: bool,
}

/// Where a context menu was requested.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ContextMenuReq {
    /// Window position of the pointer.
    pub position: Point<Pixels>,
    /// World point under the pointer (canvas menus: "Paste here").
    pub world: Option<pen_render::Vec2>,
}

/// Process-wide copy of the last clipboard text written by OpenPen (used
/// when the system clipboard cannot be read).
#[derive(Default)]
struct InternalClipboard(Option<String>);

impl Global for InternalClipboard {}

impl EditorState {
    pub fn new(doc: SceneDocument, name: impl Into<SharedString>, path: Option<PathBuf>) -> Self {
        let mut s = Self {
            doc,
            path,
            name: name.into(),
            loading: false,
            load_error: None,
            selection: Vec::new(),
            hover: None,
            expanded: HashSet::new(),
            tool: Tool::Move,
            group_choice: [Tool::Move, Tool::Rectangle],
            zoom: 1.0,
            fit_requested: true,
            fit_selection_requested: false,
            view_insets: [0.0; 4],
            rows: Vec::new(),
            context_menu: None,
            rename_request: None,
            reveal_tick: 0,
            sel_snap: (Vec::new(), Vec::new()),
            exp_snap: Vec::new(),
            agent_fx: AgentFx::new(),
            fill_edit: None,
            fill_handle_drag: false,
            assets_watch: false,
            path_mode: false,
            path_cut: false,
            path_done_requested: false,
        };
        s.reset_tree();
        s
    }

    /// Swaps in a freshly loaded document.
    pub fn replace_doc(&mut self, doc: SceneDocument, cx: &mut Context<Self>) {
        self.doc = doc;
        self.loading = false;
        self.selection.clear();
        self.hover = None;
        self.fit_requested = true;
        self.reset_tree();
        cx.notify();
    }

    fn reset_tree(&mut self) {
        self.expanded.clear();
        // Pen opens with the first top-level frame expanded one level.
        self.expanded
            .extend(self.doc.roots().iter().copied().take(1));
        self.reflatten();
    }

    pub fn is_dirty(&self) -> bool {
        self.doc.is_dirty()
    }

    pub fn rows(&self) -> &[FlatRow] {
        &self.rows
    }

    fn reflatten(&mut self) {
        self.rows = flatten(&self.doc, &self.expanded);
        self.snapshot();
    }

    /// Records the id paths of the selection and of the expanded rows.
    fn snapshot(&mut self) {
        let scene = &self.doc.scene;
        self.sel_snap = (
            self.selection.clone(),
            self.selection
                .iter()
                .map(|&id| scene.id_path(key_of(id)))
                .collect(),
        );
        self.exp_snap = self
            .expanded
            .iter()
            .filter_map(|&id| Some((id, scene.id_path(key_of(id))?)))
            .collect();
    }

    /// After an edit: nodes whose key now belongs to another node (or to
    /// nothing) are looked up again by id path.
    fn remap_keys(&mut self) {
        let scene = &self.doc.scene;
        let moved = |id: LayerId, path: &str| -> Option<Option<LayerId>> {
            if scene.id_path(key_of(id)).as_deref() == Some(path) {
                None
            } else {
                Some(scene.find(path).map(id_of))
            }
        };
        if self.selection == self.sel_snap.0 {
            let mut out = Vec::with_capacity(self.selection.len());
            for (&id, path) in self.sel_snap.0.iter().zip(&self.sel_snap.1) {
                match path.as_deref().map(|p| moved(id, p)) {
                    Some(Some(Some(n))) => out.push(n),
                    Some(Some(None)) => {}
                    _ => out.push(id),
                }
            }
            self.selection = out;
        }
        let snap = std::mem::take(&mut self.exp_snap);
        for (id, path) in &snap {
            if let Some(found) = moved(*id, path) {
                self.expanded.remove(id);
                if let Some(n) = found {
                    self.expanded.insert(n);
                }
            }
        }
    }

    /// After document edits: incremental layout + display list, drop
    /// selected nodes that no longer exist, refresh inspector snapshots.
    pub fn changed(&mut self, cx: &mut Context<Self>) {
        self.doc.flush();
        self.remap_keys();
        let doc = &self.doc;
        self.selection.retain(|id| doc.exists(*id));
        if self.hover.is_some_and(|h| !doc.exists(h)) {
            self.hover = None;
        }
        self.expanded.retain(|id| doc.exists(*id));
        self.doc.refresh_props(&self.selection);
        self.reflatten();
        cx.notify();
    }

    fn selection_changed(&mut self, cx: &mut Context<Self>) {
        self.reveal_tick += 1;
        let mut grew = false;
        for &id in &self.selection {
            grew |= reveal(&self.doc, &mut self.expanded, id);
        }
        if grew {
            self.reflatten();
        }
        self.doc.refresh_props(&self.selection);
        self.snapshot();
        cx.notify();
    }

    pub fn toggle_expanded(&mut self, id: LayerId, cx: &mut Context<Self>) {
        if !self.expanded.remove(&id) {
            self.expanded.insert(id);
        }
        self.reflatten();
        cx.notify();
    }

    /// Alt-click on a chevron: opens or closes `id` and its whole subtree.
    pub fn toggle_expanded_deep(&mut self, id: LayerId, cx: &mut Context<Self>) {
        let all = containers_below(&self.doc, id);
        if self.expanded.contains(&id) {
            for c in all {
                self.expanded.remove(&c);
            }
        } else {
            self.expanded.extend(all);
        }
        self.reflatten();
        cx.notify();
    }

    pub fn set_selection(&mut self, sel: Vec<LayerId>, cx: &mut Context<Self>) {
        if sel != self.selection {
            self.selection = sel;
            self.selection_changed(cx);
        }
    }

    pub fn select(&mut self, id: LayerId, additive: bool, cx: &mut Context<Self>) {
        if additive {
            if let Some(i) = self.selection.iter().position(|&s| s == id) {
                self.selection.remove(i);
            } else {
                self.selection.push(id);
            }
        } else {
            self.selection = vec![id];
        }
        self.selection_changed(cx);
    }

    pub fn clear_selection(&mut self, cx: &mut Context<Self>) {
        if !self.selection.is_empty() {
            self.selection.clear();
            self.selection_changed(cx);
        }
    }

    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        // Siblings of the current selection, or every top-level node.
        let parent = self.selection.first().and_then(|&id| self.doc.parent(id));
        self.selection = match parent {
            Some(p) => self.doc.children(p).to_vec(),
            None => self.doc.roots().to_vec(),
        };
        self.selection_changed(cx);
    }

    pub fn select_parent(&mut self, cx: &mut Context<Self>) {
        let mut parents: Vec<LayerId> = Vec::new();
        for p in self.selection.iter().filter_map(|&id| self.doc.parent(id)) {
            if !parents.contains(&p) {
                parents.push(p);
            }
        }
        if !parents.is_empty() {
            self.selection = parents;
            self.selection_changed(cx);
        }
    }

    pub fn select_children(&mut self, cx: &mut Context<Self>) {
        let kids: Vec<LayerId> = self
            .selection
            .iter()
            .flat_map(|&id| self.doc.children(id).iter().copied())
            .collect();
        if !kids.is_empty() {
            for &id in &self.selection {
                self.expanded.insert(id);
            }
            self.selection = kids;
            self.reflatten();
            self.selection_changed(cx);
        }
    }

    pub fn is_selected(&self, id: LayerId) -> bool {
        self.selection.contains(&id)
    }

    pub fn set_hover(&mut self, id: Option<LayerId>, cx: &mut Context<Self>) {
        if self.hover != id {
            self.hover = id;
            cx.notify();
        }
    }

    pub fn set_tool(&mut self, tool: Tool, cx: &mut Context<Self>) {
        if self.path_cut {
            self.path_cut = false;
            cx.notify();
        }
        if self.tool != tool {
            self.tool = tool;
            if let Some(g) = crate::toolbar::group_of(tool) {
                self.group_choice[g] = tool;
            }
            cx.notify();
        }
    }

    pub fn set_zoom(&mut self, z: f32, cx: &mut Context<Self>) {
        let z = zoom::clamp(z);
        if (z - self.zoom).abs() > f32::EPSILON {
            self.zoom = z;
            cx.notify();
        }
    }

    // ---------------------------------------------------------------- edits

    /// Applies an inspector edit to every selected node (one undo step).
    pub fn apply(&mut self, edit: PropEdit, cx: &mut Context<Self>) {
        if self.selection.is_empty() {
            return;
        }
        self.doc.begin();
        for &id in &self.selection {
            self.doc.apply(id, &edit);
        }
        self.doc.commit();
        self.changed(cx);
    }

    pub fn set_fill_edit(&mut self, t: Option<FillEditTarget>, cx: &mut Context<Self>) {
        if self.fill_edit != t {
            self.fill_edit = t;
            cx.notify();
        }
    }

    /// Polls background image decodes (a new or replaced image fill) until
    /// they finish, swapping the decoded images in.
    pub fn watch_assets(&mut self, cx: &mut Context<Self>) {
        if self.assets_watch {
            return;
        }
        self.assets_watch = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(30))
                    .await;
                let pending = this.update(cx, |s, cx| {
                    if s.doc.poll_assets() {
                        cx.notify();
                    }
                    s.doc.has_pending_assets()
                });
                if !matches!(pending, Ok(true)) {
                    break;
                }
            }
            let _ = this.update(cx, |s, _| s.assets_watch = false);
        })
        .detach();
    }

    /// Picks up image and script files changed on disk (window activation).
    pub fn reload_changed_assets(&mut self, cx: &mut Context<Self>) {
        if self.doc.reload_changed_assets() {
            cx.notify();
            self.watch_assets(cx);
        }
        self.reload_scripts(cx);
    }

    /// Re-runs scripts whose file changed and re-reads the selected script's
    /// header (the inspector's "Reload script").
    pub fn reload_scripts(&mut self, cx: &mut Context<Self>) {
        self.doc.refresh_scripts();
        self.doc.refresh_props(&self.selection);
        cx.notify();
    }

    /// Opens an undo block spanning several `changed` calls (a drag).
    pub fn begin_gesture(&mut self) {
        self.doc.begin();
    }

    pub fn end_gesture(&mut self, cx: &mut Context<Self>) {
        self.doc.commit();
        self.changed(cx);
    }

    pub fn undo(&mut self, cx: &mut Context<Self>) {
        if self.doc.undo() {
            self.changed(cx);
        }
    }

    pub fn redo(&mut self, cx: &mut Context<Self>) {
        if self.doc.redo() {
            self.changed(cx);
        }
    }

    pub fn set_visible(&mut self, id: LayerId, v: bool, cx: &mut Context<Self>) {
        self.doc.begin();
        self.doc.set_visible(id, v);
        self.doc.commit();
        self.changed(cx);
    }

    pub fn set_locked(&mut self, id: LayerId, v: bool, cx: &mut Context<Self>) {
        self.doc.set_locked(id, v);
        cx.notify();
    }

    pub fn rename(&mut self, id: LayerId, name: &str, cx: &mut Context<Self>) {
        self.doc.begin();
        self.doc.rename(id, name);
        self.doc.commit();
        self.changed(cx);
    }

    fn keys(&self) -> Vec<pen_doc::NodeKey> {
        self.selection.iter().map(|&i| key_of(i)).collect()
    }

    pub fn delete_selection(&mut self, cx: &mut Context<Self>) {
        // A selected gradient stop goes first (Pen).
        if let Some(t) = self.fill_edit
            && let Some(i) = t.stop
        {
            self.fill_edit = Some(FillEditTarget { stop: None, ..t });
            self.apply(
                PropEdit::Paint {
                    stroke: t.stroke,
                    index: t.index,
                    op: PaintOp::RemoveStop(i),
                },
                cx,
            );
            return;
        }
        let keys = self.keys();
        if self.doc.delete(&keys) > 0 {
            self.selection.clear();
            self.changed(cx);
        }
    }

    pub fn duplicate_selection(&mut self, cx: &mut Context<Self>) {
        let keys = self.keys();
        let made = self.doc.duplicate(&keys);
        if !made.is_empty() {
            self.selection = made.into_iter().map(id_of).collect();
            self.changed(cx);
        }
    }

    pub fn group_selection(&mut self, cx: &mut Context<Self>) {
        let keys = self.keys();
        if let Some(g) = self.doc.group(&keys) {
            self.selection = vec![id_of(g)];
            self.changed(cx);
            self.selection_changed(cx);
        }
    }

    pub fn ungroup_selection(&mut self, cx: &mut Context<Self>) {
        let mut out = Vec::new();
        self.doc.begin();
        for k in self.keys() {
            out.extend(self.doc.ungroup(k));
        }
        self.doc.commit();
        if !out.is_empty() {
            self.selection = out.into_iter().map(id_of).collect();
            self.changed(cx);
        }
    }

    // ------------------------------------------------------------ agents

    /// Runs an agent's `execute` script (one undo step, or nothing on error)
    /// and refreshes the panels like any other edit. Touched nodes flash and
    /// the agent's cursor visits them; `view` is the visible world rect
    /// (where a new cursor appears).
    ///
    /// `skip_touched`: leading `resp.touched` entries already flashed while
    /// the script streamed (finalizing a stream, see `agent::stream`).
    pub fn run_agent_script(
        &mut self,
        engine: &mut pen_script::ScriptEngine,
        agent: &AgentInfo,
        req: pen_script::ExecuteRequest,
        view: pen_render::Rect,
        skip_touched: usize,
        cx: &mut Context<Self>,
    ) -> pen_script::ExecuteResponse {
        let resp = self.doc.run_script(engine, &mut AppScriptHost, req);
        self.changed(cx);
        let touched = resp.touched.get(skip_touched..).unwrap_or_default();
        self.flash_agent(agent, touched, view);
        cx.notify();
        resp
    }

    /// Feeds a streamed `execute` prefix (`SceneDocument::stream_partial`):
    /// the new complete statements show up live, uncommitted, with the
    /// panels refreshed and the touched nodes flashing.
    pub fn run_agent_partial(
        &mut self,
        engine: &mut pen_script::ScriptEngine,
        agent: &AgentInfo,
        prefix: &str,
        view: pen_render::Rect,
        cx: &mut Context<Self>,
    ) -> pen_script::PartialStatus {
        let status = self.doc.stream_partial(engine, &agent.client_id, prefix);
        self.changed(cx);
        self.flash_agent(agent, &status.touched, view);
        cx.notify();
        status
    }

    /// Reverts an agent's open stream on this document.
    pub fn cancel_agent_stream(
        &mut self,
        engine: &mut pen_script::ScriptEngine,
        agent: &str,
        cx: &mut Context<Self>,
    ) {
        self.doc.stream_cancel(engine, agent);
        self.changed(cx);
        cx.notify();
    }

    fn flash_agent(
        &mut self,
        agent: &AgentInfo,
        touched: &[pen_script::Touch],
        view: pen_render::Rect,
    ) {
        let nodes = touched_nodes(&self.doc, touched);
        let spawn = pen_render::Vec2::new(view.x + view.w * 0.5, view.y + view.h * 0.5);
        self.agent_fx.touch(
            &agent.client_id,
            &agent.name,
            agent.color,
            &nodes,
            spawn,
            std::time::Instant::now(),
        );
    }

    // ------------------------------------------------------- clipboard

    /// Copies the selection as `.pen` JSON to the system clipboard.
    pub fn copy_selection(&mut self, cx: &mut Context<Self>) -> bool {
        let keys = self.keys();
        let nodes = self.doc.copy_nodes(&keys);
        if nodes.is_empty() {
            return false;
        }
        let text = crate::clipboard::encode(&nodes, Some(self.doc.scene.document()));
        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
        cx.set_global(InternalClipboard(Some(text)));
        true
    }

    pub fn cut_selection(&mut self, cx: &mut Context<Self>) {
        if self.copy_selection(cx) {
            self.delete_selection(cx);
        }
    }

    /// Pastes the clipboard (`view`: visible world rect; `at`: "Paste here"
    /// point). One undo step; selects the pasted nodes.
    pub fn paste(
        &mut self,
        view: pen_render::Rect,
        at: Option<pen_render::Vec2>,
        cx: &mut Context<Self>,
    ) {
        let text = cx.read_from_clipboard().and_then(|c| c.text()).or_else(|| {
            cx.try_global::<InternalClipboard>()
                .and_then(|c| c.0.clone())
        });
        let Some(clip) = text.as_deref().and_then(crate::clipboard::decode) else {
            return;
        };
        let sel = self.keys();
        self.doc.begin();
        self.doc
            .add_missing_variables(clip.variables, &clip.default_theme);
        let made = self.doc.paste(clip.nodes, &sel, view, at);
        self.doc.commit();
        if !made.is_empty() {
            self.selection = made.into_iter().map(id_of).collect();
        }
        self.changed(cx);
        self.selection_changed(cx);
    }

    pub fn arrange(&mut self, how: Arrange, cx: &mut Context<Self>) {
        let keys = self.keys();
        if self.doc.arrange(&keys, how) {
            self.changed(cx);
        }
    }

    /// Layer-tree drop (see [`SceneDocument::move_layers`]).
    pub fn move_layers(
        &mut self,
        ids: &[LayerId],
        parent: Option<LayerId>,
        index: usize,
        cx: &mut Context<Self>,
    ) {
        let keys: Vec<_> = ids.iter().map(|&i| key_of(i)).collect();
        let moved = self.doc.move_layers(&keys, parent.map(key_of), index);
        if !moved.is_empty() {
            if let Some(p) = parent {
                self.expanded.insert(p);
            }
            self.selection = moved.into_iter().map(id_of).collect();
            self.changed(cx);
        }
    }

    /// Hides the selection, or shows it when every selected node is hidden.
    pub fn toggle_visible_selection(&mut self, cx: &mut Context<Self>) {
        if self.selection.is_empty() {
            return;
        }
        let all_hidden = self
            .selection
            .iter()
            .all(|&id| self.doc.info(id).is_some_and(|i| !i.visible));
        self.doc.begin();
        for &id in &self.selection {
            self.doc.set_visible(id, all_hidden);
        }
        self.doc.commit();
        self.changed(cx);
    }

    pub fn open_context_menu(&mut self, req: ContextMenuReq, cx: &mut Context<Self>) {
        self.context_menu = Some(req);
        cx.notify();
    }

    pub fn close_context_menu(&mut self, cx: &mut Context<Self>) {
        if self.context_menu.take().is_some() {
            cx.notify();
        }
    }

    /// Arrow-key nudge of the selection by (`dx`, `dy`) world units.
    pub fn nudge(&mut self, dx: f32, dy: f32, cx: &mut Context<Self>) {
        let keys = crate::select::top_level_only(
            &self.doc as &dyn crate::select::PickTree<pen_doc::NodeKey>,
            &self.keys(),
        );
        if keys.is_empty() {
            return;
        }
        self.doc.begin();
        for k in keys {
            let Some(g0) = self.doc.box_geom(k) else {
                continue;
            };
            // World delta -> parent space.
            let inv = self.doc.parent_world(k).matrix2.inverse();
            let d = inv * pen_render::Vec2::new(dx, dy);
            let g = crate::geom::BoxGeom {
                x: g0.x + d.x,
                y: g0.y + d.y,
                ..g0
            };
            self.doc.set_box(k, &g0, &g);
        }
        self.doc.commit();
        self.changed(cx);
    }
}
