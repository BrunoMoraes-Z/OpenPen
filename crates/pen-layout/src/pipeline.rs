//! Convenience wrapper: scene -> layout -> display list, incrementally.

use pen_doc::SceneDoc;
use pen_render::{DisplayList, NodeKey, Rect};

use crate::assets::AssetResolver;
use crate::display::{DisplayListBuilder, node_world_rect};
use crate::layout::LayoutEngine;
use crate::script::{ScriptExpander, ScriptRunner};
use crate::text::TextEngine;

/// Owns the text engine, layout results, display-list builder and the
/// current [`DisplayList`]. `Send`: a pipeline built on a worker thread can be
/// moved to the UI thread.
pub struct Pipeline {
    pub text: TextEngine,
    pub layout: LayoutEngine,
    pub display: DisplayListBuilder,
    pub list: DisplayList,
    /// Expands `script` nodes (none: they draw a placeholder). See [`Self::set_script_runner`].
    pub scripts: Option<ScriptExpander>,
}

/// What [`Pipeline::update`] recomputed.
#[derive(Debug, Default, Clone)]
pub struct UpdateStats {
    pub relaid: Vec<NodeKey>,
    pub rebuilt_chunks: Vec<NodeKey>,
}

#[allow(dead_code)]
fn assert_send() {
    fn is_send<T: Send>() {}
    is_send::<Pipeline>();
    is_send::<(SceneDoc, Pipeline)>();
}

impl Pipeline {
    pub fn new(text: TextEngine, assets: Box<dyn AssetResolver>) -> Self {
        Self {
            text,
            layout: LayoutEngine::new(),
            display: DisplayListBuilder::new(assets),
            list: DisplayList::default(),
            scripts: None,
        }
    }

    /// Plugs the engine that runs `script` nodes (e.g. `pen_script::ScriptNodeRunner`).
    /// Takes effect on the next [`Self::rebuild`] (or [`Self::refresh_scripts`]).
    pub fn set_script_runner(&mut self, runner: Box<dyn ScriptRunner>) {
        self.scripts = Some(ScriptExpander::new(runner));
    }

    /// Plugs the engine that renders `shader` fills and effects (e.g.
    /// `pen_shader::ShaderEngine`). Takes effect on the next build of each chunk.
    pub fn set_shader_renderer(&mut self, r: std::sync::Arc<dyn crate::shader::ShaderRenderer>) {
        self.display.set_shader_renderer(r);
    }

    /// Full layout + display list (drops the scene's pending changes).
    pub fn rebuild(&mut self, scene: &mut SceneDoc) -> &DisplayList {
        let _ = scene.take_changes();
        self.layout.layout_all(scene, &mut self.text);
        if let Some(sx) = &mut self.scripts {
            let full = pen_doc::Changes {
                full: true,
                ..Default::default()
            };
            if sx.sync(scene, &self.layout, &full, &[]) {
                let more = scene.take_changes();
                self.layout.update(scene, &more, &mut self.text);
            }
        }
        self.list = self.display.build(scene, &self.layout);
        &self.list
    }

    /// Re-reads every script file and updates the outputs that changed (call it when the
    /// app regains focus). True if `list` changed.
    pub fn refresh_scripts(&mut self, scene: &mut SceneDoc) -> bool {
        let Some(sx) = &mut self.scripts else {
            return false;
        };
        if !sx.sync_all(scene, &self.layout) {
            return false;
        }
        !self.update(scene).rebuilt_chunks.is_empty()
    }

    /// Consumes the scene's change set and updates only what changed.
    pub fn update(&mut self, scene: &mut SceneDoc) -> UpdateStats {
        let mut changes = scene.take_changes();
        if changes.is_empty() {
            return UpdateStats::default();
        }
        let mut relaid = self.layout.update(scene, &changes, &mut self.text);
        // Scripts whose size, inputs or file changed: attach the new output, then lay
        // out the affected roots again.
        if let Some(sx) = &mut self.scripts
            && sx.sync(scene, &self.layout, &changes, &relaid)
        {
            let more = scene.take_changes();
            let again = self.layout.update(scene, &more, &mut self.text);
            changes.full |= more.full;
            changes.roots_changed |= more.roots_changed;
            changes.dirty.extend(more.dirty);
            relaid.extend(again);
            relaid.sort();
            relaid.dedup();
        }
        let rebuilt_chunks =
            self.display
                .update(scene, &self.layout, &changes, &relaid, &mut self.list);
        UpdateStats {
            relaid,
            rebuilt_chunks,
        }
    }

    /// Rebuilds the chunks waiting for images that finished loading in the background
    /// (resolvers like [`crate::FsAssets::deferred`]). True if `list` changed.
    pub fn refresh_assets(&mut self, scene: &SceneDoc) -> bool {
        !self
            .display
            .refresh_assets(scene, &self.layout, &mut self.list)
            .is_empty()
    }

    /// Rebuilds the chunks whose image files changed on disk since they were
    /// loaded (call it when the app regains focus). True if `list` changed;
    /// with a deferred resolver the new images then arrive through
    /// [`Self::refresh_assets`].
    pub fn reload_changed_assets(&mut self, scene: &SceneDoc) -> bool {
        !self
            .display
            .reload_changed_assets(scene, &self.layout, &mut self.list)
            .is_empty()
    }

    /// True while some image is still loading (poll [`Self::refresh_assets`]).
    pub fn has_pending_assets(&self) -> bool {
        self.display.has_pending_assets()
    }

    /// World bounds of a node's layout box.
    pub fn node_rect(&self, scene: &SceneDoc, k: NodeKey) -> Option<Rect> {
        node_world_rect(scene, &self.layout, k)
    }

    /// A display list with only the subtree of `k` (for exporting one node).
    pub fn node_list(&mut self, scene: &SceneDoc, k: NodeKey) -> DisplayList {
        self.display
            .build_chunk(scene, &self.layout, k)
            .into_iter()
            .collect()
    }
}
