//! Convenience wrapper: scene -> layout -> display list, incrementally.

use pen_doc::SceneDoc;
use pen_render::{DisplayList, NodeKey, Rect};

use crate::assets::AssetResolver;
use crate::display::{DisplayListBuilder, node_world_rect};
use crate::layout::LayoutEngine;
use crate::text::TextEngine;

/// Owns the text engine, layout results, display-list builder and the
/// current [`DisplayList`]. `Send`: a pipeline built on a worker thread can be
/// moved to the UI thread.
pub struct Pipeline {
    pub text: TextEngine,
    pub layout: LayoutEngine,
    pub display: DisplayListBuilder,
    pub list: DisplayList,
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
        }
    }

    /// Full layout + display list (drops the scene's pending changes).
    pub fn rebuild(&mut self, scene: &mut SceneDoc) -> &DisplayList {
        let _ = scene.take_changes();
        self.layout.layout_all(scene, &mut self.text);
        self.list = self.display.build(scene, &self.layout);
        &self.list
    }

    /// Consumes the scene's change set and updates only what changed.
    pub fn update(&mut self, scene: &mut SceneDoc) -> UpdateStats {
        let changes = scene.take_changes();
        if changes.is_empty() {
            return UpdateStats::default();
        }
        let relaid = self.layout.update(scene, &changes, &mut self.text);
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
