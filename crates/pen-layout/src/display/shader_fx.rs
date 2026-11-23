//! Shader fills and effects in the display-list builder (see [`crate::shader`]).

use std::hash::{DefaultHasher, Hash, Hasher};

use pen_doc::ShaderSpec;

use super::*;
use crate::shader::{ShaderLookup, ShaderNeeds, ShaderRenderer, ShaderRequest};

/// Drawn where a shader failed to compile or run.
pub const SHADER_ERROR: Color = Color::rgba(0.9, 0.1, 0.5, 0.25);

/// Context of the fills being drawn (for `pencil:object` / `pencil:background`).
pub(super) struct FillCtx {
    /// Address and length of the node's fill list (identifies the fill being painted).
    fills_ptr: usize,
    fills_len: usize,
    fills: SmallVec<[PaintLayer; 1]>,
    shape: Shape,
    backdrop: Option<Arc<DisplayList>>,
    backdrop_hash: u64,
}

impl DisplayListBuilder {
    /// Plugs the shader engine (without one, shader fills and effects are skipped).
    pub fn set_shader_renderer(&mut self, r: Arc<dyn ShaderRenderer>) {
        self.shaders = Some(r);
    }

    pub(super) fn begin_fills(
        &mut self,
        cx: &Cx<'_>,
        fills: &[PaintLayer],
        shape: &Shape,
        world: Affine2,
    ) {
        self.fill_ctx = None;
        let Some(r) = self.shaders.clone() else {
            return;
        };
        let mut needs = ShaderNeeds::default();
        let mut any = false;
        for f in fills {
            if let PaintSpec::Shader(sh) = &f.paint {
                any = true;
                let n = r.needs(&sh.url, cx.scene.base_dir());
                needs.object |= n.object;
                needs.backdrop |= n.backdrop;
            }
        }
        if !any {
            return;
        }
        let (backdrop, backdrop_hash) = if needs.backdrop {
            backdrop_list(&cx.items[..self.node_start], world)
        } else {
            (None, 0)
        };
        self.fill_ctx = Some(FillCtx {
            fills_ptr: fills.as_ptr() as usize,
            fills_len: fills.len(),
            fills: fills.iter().cloned().collect(),
            shape: shape.clone(),
            backdrop,
            backdrop_hash,
        });
    }

    /// Paint of a shader fill: the rendered image stretched over the node box.
    pub(super) fn shader_paint(
        &mut self,
        p: &PaintLayer,
        sh: &ShaderSpec,
        size: Vec2,
        alpha: f32,
    ) -> Paint {
        let Some(r) = self.shaders.clone() else {
            return Paint::Solid(Color::TRANSPARENT);
        };
        let base_dir = self.base_dir.clone();
        // Which fill of the node this is (None for strokes).
        let index = self.fill_ctx.as_ref().and_then(|c| {
            let at = p as *const PaintLayer as usize;
            let sz = std::mem::size_of::<PaintLayer>();
            (at >= c.fills_ptr && at < c.fills_ptr + c.fills_len * sz)
                .then(|| (at - c.fills_ptr) / sz)
        });
        let needs = r.needs(&sh.url, base_dir.as_deref());
        let mut h = DefaultHasher::new();
        hash_shader(sh, &mut h);
        size.x.to_bits().hash(&mut h);
        size.y.to_bits().hash(&mut h);
        let mut object = None;
        let mut backdrop = None;
        let mut shape = None;
        if let (Some(i), Some(ctx)) = (index, self.fill_ctx.as_ref()) {
            let shape_s = ctx.shape.clone();
            format!("{shape_s:?}").hash(&mut h);
            if needs.object {
                // The fills under this one, over transparent.
                let lower: SmallVec<[PaintLayer; 1]> = ctx.fills[..i].iter().cloned().collect();
                format!("{lower:?}").hash(&mut h);
                let saved = self.fill_ctx.take();
                let paints: SmallVec<[Paint; 1]> =
                    lower.iter().map(|f| self.paint(f, size, 1.0)).collect();
                self.fill_ctx = saved;
                object = Some(Arc::new(single_item_list(DisplayItem::Shape {
                    node: NodeKey(0),
                    transform: Affine2::IDENTITY,
                    shape: shape_s.clone(),
                    fills: paints,
                    stroke: None,
                    shadows: SmallVec::new(),
                })));
            }
            if needs.backdrop
                && let Some(ctx) = self.fill_ctx.as_ref()
            {
                ctx.backdrop_hash.hash(&mut h);
                backdrop = ctx.backdrop.clone();
            }
            shape = Some(shape_s);
        }
        let req = ShaderRequest {
            url: sh.url.clone(),
            base_dir,
            uniforms: sh.uniforms.clone(),
            size,
            region: Rect::new(0.0, 0.0, size.x, size.y),
            shape: shape.or(Some(Shape::Rect {
                size,
                radii: CornerRadii::default(),
            })),
            object,
            backdrop,
            key: h.finish(),
        };
        match r.render(&req) {
            ShaderLookup::Ready(image) => Paint::Image {
                image,
                fit: ImageFit::Stretch,
                opacity: sh.opacity * alpha,
                scale: 1.0,
            },
            ShaderLookup::Pending => {
                self.pending_roots.insert(self.cur_root);
                Paint::Solid(Color::TRANSPARENT)
            }
            ShaderLookup::Failed(_) => Paint::Solid(SHADER_ERROR.with_alpha_mul(alpha)),
        }
    }

    /// Replaces the items of a node with `shader` effects (`cx.items[node_start..]`, whose
    /// bounds were accumulated in a pushed `cx.bounds` slot) by the chained shader output.
    pub(super) fn apply_shader_effects(
        &mut self,
        cx: &mut Cx<'_>,
        shaders: &[ShaderSpec],
        world: Affine2,
        size: Vec2,
        node_start: usize,
    ) {
        let content_bounds = cx.bounds.pop().flatten();
        let Some(r) = self.shaders.clone() else {
            if let Some(b) = content_bounds {
                cx.add_bounds(b);
            }
            return;
        };
        let Some(world_bounds) = content_bounds else {
            return;
        };
        let inv = world.inverse();
        // Visual bounds in local space, including the node box.
        let region = world_bounds
            .transformed(&inv)
            .union(&Rect::new(0.0, 0.0, size.x, size.y));
        let region = Rect::new(
            region.x.floor(),
            region.y.floor(),
            region.w.ceil() + 1.0,
            region.h.ceil() + 1.0,
        );
        let base_dir = self.base_dir.clone();
        let mut content: Vec<DisplayItem> = cx.items[node_start..]
            .iter()
            .map(|it| to_local(it, &inv))
            .collect();
        let mut hc = DefaultHasher::new();
        hash_items(&content, &mut hc);
        let mut content_hash = hc.finish();
        let mut out: Option<Arc<ImageData>> = None;
        let mut backdrop_cache: Option<(Option<Arc<DisplayList>>, u64)> = None;
        for sh in shaders {
            let needs = r.needs(&sh.url, base_dir.as_deref());
            let mut h = DefaultHasher::new();
            hash_shader(sh, &mut h);
            content_hash.hash(&mut h);
            for v in [size.x, size.y, region.x, region.y, region.w, region.h] {
                v.to_bits().hash(&mut h);
            }
            let backdrop = if needs.backdrop {
                let (b, bh) = backdrop_cache
                    .get_or_insert_with(|| backdrop_list(&cx.items[..node_start], world))
                    .clone();
                bh.hash(&mut h);
                b
            } else {
                None
            };
            let req = ShaderRequest {
                url: sh.url.clone(),
                base_dir: base_dir.clone(),
                uniforms: sh.uniforms.clone(),
                size,
                region,
                shape: None,
                object: Some(Arc::new(list_of(content.clone(), region))),
                backdrop,
                key: h.finish(),
            };
            match r.render(&req) {
                ShaderLookup::Ready(img) => {
                    content_hash = img.id;
                    content = vec![image_item(img.clone(), region)];
                    out = Some(img);
                }
                ShaderLookup::Pending => {
                    self.pending_roots.insert(self.cur_root);
                    out = None;
                    break;
                }
                ShaderLookup::Failed(_) => {
                    out = None;
                    break;
                }
            }
        }
        match out {
            Some(img) => {
                cx.items.truncate(node_start);
                let mut item = image_item(img, region);
                if let DisplayItem::Shape { transform, .. } = &mut item {
                    *transform = world * *transform;
                }
                cx.items.push(item);
                cx.add_bounds(region.transformed(&world));
            }
            // Pending or failed: the node as if it had no shader effect.
            None => cx.add_bounds(world_bounds),
        }
    }
}

/// Local-space image item covering `region`.
fn image_item(image: Arc<ImageData>, region: Rect) -> DisplayItem {
    DisplayItem::Shape {
        node: NodeKey(0),
        transform: Affine2::from_translation(Vec2::new(region.x, region.y)),
        shape: Shape::Rect {
            size: Vec2::new(region.w, region.h),
            radii: CornerRadii::default(),
        },
        fills: smallvec![Paint::Image {
            image,
            fit: ImageFit::Stretch,
            opacity: 1.0,
            scale: 1.0,
        }],
        stroke: None,
        shadows: SmallVec::new(),
    }
}

fn single_item_list(item: DisplayItem) -> DisplayList {
    list_of(vec![item], Rect::new(-1e6, -1e6, 2e6, 2e6))
}

fn list_of(items: Vec<DisplayItem>, bounds: Rect) -> DisplayList {
    DisplayList {
        chunks: vec![Arc::new(Chunk {
            root: NodeKey(0),
            bounds: outset(bounds, 1e5),
            version: 0,
            items,
        })],
    }
}

/// Items drawn before the node (same chunk), moved to its local space, with the
/// scopes still open at that point closed.
fn backdrop_list(prefix: &[DisplayItem], world: Affine2) -> (Option<Arc<DisplayList>>, u64) {
    if prefix.is_empty() {
        return (None, 0);
    }
    let inv = world.inverse();
    let mut items: Vec<DisplayItem> = prefix.iter().map(|it| to_local(it, &inv)).collect();
    let mut open: Vec<bool> = Vec::new(); // true = clip
    for it in prefix {
        match it {
            DisplayItem::PushClip { .. } => open.push(true),
            DisplayItem::PushLayer { .. } => open.push(false),
            DisplayItem::PopClip | DisplayItem::PopLayer => {
                open.pop();
            }
            _ => {}
        }
    }
    while let Some(clip) = open.pop() {
        items.push(if clip {
            DisplayItem::PopClip
        } else {
            DisplayItem::PopLayer
        });
    }
    let mut h = DefaultHasher::new();
    hash_items(&items, &mut h);
    let hash = h.finish();
    (
        Some(Arc::new(list_of(items, Rect::new(-1e6, -1e6, 2e6, 2e6)))),
        hash,
    )
}

/// An item with its transform pre-multiplied by `inv` (world -> node local).
fn to_local(it: &DisplayItem, inv: &Affine2) -> DisplayItem {
    let mut it = it.clone();
    match &mut it {
        DisplayItem::Shape { transform, .. }
        | DisplayItem::Text { transform, .. }
        | DisplayItem::PushClip { transform, .. } => *transform = *inv * *transform,
        DisplayItem::PushLayer { bounds, .. } => *bounds = bounds.transformed(inv),
        DisplayItem::PopClip | DisplayItem::PopLayer => {}
    }
    it
}

fn hash_shader(sh: &ShaderSpec, h: &mut DefaultHasher) {
    sh.url.hash(h);
    crate::script::hash_params(&sh.uniforms, h);
}

fn hash_paint(p: &Paint, h: &mut DefaultHasher) {
    match p {
        Paint::Image {
            image,
            fit,
            opacity,
            scale,
        } => {
            image.id.hash(h);
            format!("{fit:?}").hash(h);
            opacity.to_bits().hash(h);
            scale.to_bits().hash(h);
        }
        other => format!("{other:?}").hash(h),
    }
}

/// Identity of a list of items (images by id, everything else by value).
fn hash_items(items: &[DisplayItem], h: &mut DefaultHasher) {
    for it in items {
        match it {
            DisplayItem::Shape {
                transform,
                shape,
                fills,
                stroke,
                shadows,
                ..
            } => {
                0u8.hash(h);
                format!("{transform:?}{shape:?}{shadows:?}").hash(h);
                for f in fills {
                    hash_paint(f, h);
                }
                if let Some(s) = stroke {
                    format!("{:?}{:?}{:?}", s.widths, s.align, s.dash).hash(h);
                    for p in &s.paints {
                        hash_paint(p, h);
                    }
                }
            }
            DisplayItem::Text {
                transform, block, ..
            } => {
                1u8.hash(h);
                format!("{transform:?}").hash(h);
                for r in &block.runs {
                    hash_paint(&r.style.paint, h);
                }
                format!("{:?}", block.lines).hash(h);
            }
            other => format!("{other:?}").hash(h),
        }
    }
}
