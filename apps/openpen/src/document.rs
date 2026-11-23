//! Document boundary of the shell.
//!
//! Panels only see `dyn DocumentSource`. Today it is [`PlaceholderDocument`];
//! the `pen-doc` adapter will implement the same two traits (layer tree +
//! properties) over the real scene graph.

use crate::inspector::model::{
    Align, Effect, EffectKind, Justify, LayoutMode, NodeProps, Paint, PaintKind, PropEdit,
    PropertySource, TextProps,
};
use crate::inspector::value::SizeValue;
use crate::layers::model::{LayerId, LayerInfo, LayerKind, LayerTreeSource, PlaceholderTree};

pub trait DocumentSource: LayerTreeSource + PropertySource {}
impl<T: LayerTreeSource + PropertySource> DocumentSource for T {}

pub struct PlaceholderDocument {
    tree: PlaceholderTree,
    props: Vec<NodeProps>,
}

impl PlaceholderDocument {
    pub fn sample() -> Self {
        let tree = PlaceholderTree::sample();
        let props = (0..tree.len() as u64)
            .map(|i| sample_props(&tree, LayerId(i)))
            .collect();
        Self { tree, props }
    }
}

fn sample_props(tree: &PlaceholderTree, id: LayerId) -> NodeProps {
    let info = tree.info(id).expect("sample id");
    let mut p = NodeProps::new(info.kind);
    let n = id.0 as f32;
    p.x = (n * 37.0) % 480.0;
    p.y = (n * 53.0) % 360.0;
    match info.kind {
        LayerKind::Frame | LayerKind::Component | LayerKind::Instance => {
            let depth = {
                let mut d = 0;
                let mut c = tree.parent(id);
                while let Some(x) = c {
                    d += 1;
                    c = tree.parent(x);
                }
                d
            };
            if depth == 0 {
                p.x = 0.0;
                p.y = 0.0;
                p.width = SizeValue::Fixed(1440.0);
                p.height = SizeValue::FitContent(None);
                p.resolved_size = (1440.0, 2160.0);
                p.layout.mode = LayoutMode::Vertical;
            } else {
                p.width = SizeValue::FillContainer(None);
                p.height = SizeValue::FitContent(None);
                p.resolved_size = (1280.0, 64.0);
                p.layout.mode = LayoutMode::Horizontal;
                p.layout.align = Align::Center;
                p.layout.justify = Justify::SpaceBetween;
            }
            p.layout.gap = 16.0;
            p.layout.padding = [24.0, 32.0, 24.0, 32.0];
            p.corner_radius = [if depth == 0 { 0.0 } else { 12.0 }; 4];
        }
        LayerKind::Text => {
            p.fills = vec![Paint::solid(0x0a0a0aff)];
            p.text = Some(TextProps {
                font_size: if info.name.len() > 20 { 56.0 } else { 16.0 },
                font_weight: if info.name.len() > 20 { 700 } else { 400 },
                ..TextProps::default()
            });
        }
        LayerKind::Rectangle => {
            p.corner_radius = [8.0; 4];
            p.fills = vec![Paint::solid(0x18181bff)];
        }
        LayerKind::Ellipse => {
            p.width = SizeValue::Fixed(40.0);
            p.height = SizeValue::Fixed(40.0);
            p.resolved_size = (40.0, 40.0);
            p.fills = vec![Paint::solid(0xe4e4e7ff)];
        }
        LayerKind::Image => {
            p.fills = vec![Paint {
                kind: PaintKind::Image,
                opacity: 1.0,
                visible: true,
            }];
            p.effects = vec![Effect::new(EffectKind::DropShadow)];
            p.corner_radius = [16.0; 4];
        }
        LayerKind::Line => {
            p.height = SizeValue::Fixed(0.0);
            p.fills.clear();
            p.strokes = vec![Paint::solid(0xe4e4e7ff)];
        }
        _ => {}
    }
    p
}

impl LayerTreeSource for PlaceholderDocument {
    fn roots(&self) -> &[LayerId] {
        self.tree.roots()
    }
    fn children(&self, id: LayerId) -> &[LayerId] {
        self.tree.children(id)
    }
    fn parent(&self, id: LayerId) -> Option<LayerId> {
        self.tree.parent(id)
    }
    fn info(&self, id: LayerId) -> Option<LayerInfo<'_>> {
        self.tree.info(id)
    }
    fn set_visible(&mut self, id: LayerId, visible: bool) {
        self.tree.set_visible(id, visible)
    }
    fn set_locked(&mut self, id: LayerId, locked: bool) {
        self.tree.set_locked(id, locked)
    }
    fn rename(&mut self, id: LayerId, name: &str) {
        self.tree.rename(id, name)
    }
}

impl PropertySource for PlaceholderDocument {
    fn props(&self, id: LayerId) -> Option<&NodeProps> {
        self.props.get(id.0 as usize)
    }
    fn apply(&mut self, id: LayerId, edit: &PropEdit) {
        if let Some(p) = self.props.get_mut(id.0 as usize) {
            p.apply(edit);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_has_props_for_every_layer() {
        let d = PlaceholderDocument::sample();
        for i in 0..d.tree.len() as u64 {
            let id = LayerId(i);
            assert_eq!(d.props(id).unwrap().kind, d.info(id).unwrap().kind);
        }
    }

    #[test]
    fn apply_goes_through() {
        let mut d = PlaceholderDocument::sample();
        let id = d.roots()[0];
        d.apply(id, &PropEdit::X(12.0));
        assert_eq!(d.props(id).unwrap().x, 12.0);
    }
}
