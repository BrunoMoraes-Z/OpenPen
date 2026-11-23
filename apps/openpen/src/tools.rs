//! Canvas tools (Pen `setActiveTool` ids) with their shortcuts and icons.

use gpui_kit::Action;
use gpui_kit::assets::IconName;

use crate::actions as a;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Tool {
    #[default]
    Move,
    Scale,
    Hand,
    Frame,
    Rectangle,
    Ellipse,
    Line,
    Polygon,
    Pen,
    Text,
    Note,
    Icon,
    Image,
    Comment,
    Eyedropper,
}

impl Tool {
    pub const ALL: [Tool; 15] = [
        Tool::Move,
        Tool::Scale,
        Tool::Hand,
        Tool::Frame,
        Tool::Rectangle,
        Tool::Ellipse,
        Tool::Line,
        Tool::Polygon,
        Tool::Pen,
        Tool::Text,
        Tool::Note,
        Tool::Icon,
        Tool::Image,
        Tool::Comment,
        Tool::Eyedropper,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Tool::Move => "Move",
            Tool::Scale => "Scale",
            Tool::Hand => "Hand tool",
            Tool::Frame => "Frame",
            Tool::Rectangle => "Rectangle",
            Tool::Ellipse => "Ellipse",
            Tool::Line => "Line",
            Tool::Polygon => "Polygon",
            Tool::Pen => "Pen",
            Tool::Text => "Text",
            Tool::Note => "Sticky note",
            Tool::Icon => "Icon",
            Tool::Image => "Image",
            Tool::Comment => "Comment",
            Tool::Eyedropper => "Eyedropper",
        }
    }

    /// Shortcuts from Pen's action registry (doc 02 §8.1). `L` for Line is an
    /// OpenPen addition (Pen has no line tool shortcut).
    pub fn shortcuts(self) -> &'static [&'static str] {
        match self {
            Tool::Move => &["v"],
            Tool::Scale => &["k"],
            Tool::Hand => &["h"],
            Tool::Frame => &["f", "a"],
            Tool::Rectangle => &["r"],
            Tool::Ellipse => &["o"],
            Tool::Line => &["l"],
            Tool::Pen => &["p"],
            Tool::Text => &["t"],
            Tool::Note => &["n"],
            Tool::Comment => &["c"],
            Tool::Eyedropper => &["i"],
            Tool::Polygon | Tool::Icon | Tool::Image => &[],
        }
    }

    /// First shortcut, upper-cased for tooltips ("R").
    pub fn shortcut_label(self) -> Option<String> {
        self.shortcuts().first().map(|s| s.to_ascii_uppercase())
    }

    pub fn icon(self) -> IconName {
        match self {
            Tool::Move => IconName::MousePointer2,
            Tool::Scale => IconName::Scaling,
            Tool::Hand => IconName::Hand,
            Tool::Frame => IconName::Frame,
            Tool::Rectangle => IconName::Square,
            Tool::Ellipse => IconName::Circle,
            Tool::Line => IconName::Slash,
            Tool::Polygon => IconName::Triangle,
            Tool::Pen => IconName::PenTool,
            Tool::Text => IconName::Type,
            Tool::Note => IconName::StickyNote,
            Tool::Icon => IconName::Shapes,
            Tool::Image => IconName::Image,
            Tool::Comment => IconName::MessageCircle,
            Tool::Eyedropper => IconName::Pipette,
        }
    }

    pub fn action_ctor(self) -> fn() -> Box<dyn Action> {
        match self {
            Tool::Move => || Box::new(a::ToolMove),
            Tool::Scale => || Box::new(a::ToolScale),
            Tool::Hand => || Box::new(a::ToolHand),
            Tool::Frame => || Box::new(a::ToolFrame),
            Tool::Rectangle => || Box::new(a::ToolRectangle),
            Tool::Ellipse => || Box::new(a::ToolEllipse),
            Tool::Line => || Box::new(a::ToolLine),
            Tool::Polygon => || Box::new(a::ToolPolygon),
            Tool::Pen => || Box::new(a::ToolPen),
            Tool::Text => || Box::new(a::ToolText),
            Tool::Note => || Box::new(a::ToolNote),
            Tool::Icon => || Box::new(a::ToolIcon),
            Tool::Image => || Box::new(a::ToolImage),
            Tool::Comment => || Box::new(a::ToolComment),
            Tool::Eyedropper => || Box::new(a::ToolEyedropper),
        }
    }

    /// Maps a dispatched action back to its tool.
    pub fn from_action(action: &dyn Action) -> Option<Tool> {
        Tool::ALL
            .into_iter()
            .find(|t| (t.action_ctor())().partial_eq(action))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_round_trip() {
        for t in Tool::ALL {
            assert_eq!(Tool::from_action((t.action_ctor())().as_ref()), Some(t));
        }
    }

    #[test]
    fn shortcut_labels() {
        assert_eq!(Tool::Rectangle.shortcut_label().as_deref(), Some("R"));
        assert_eq!(Tool::Frame.shortcut_label().as_deref(), Some("F"));
        assert_eq!(Tool::Icon.shortcut_label(), None);
    }
}
