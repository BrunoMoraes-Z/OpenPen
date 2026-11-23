//! Fonts: the text faces bundled in the binary (`pen_layout::bundled_fonts`,
//! Inter) are registered with GPUI's text system at startup so the canvas
//! draws with the same faces pen-layout measured with. Icons need no font:
//! pen-layout turns them into vector paths.

use std::borrow::Cow;
use std::sync::Arc;

use gpui_kit::{App, Global};

/// Family of the bundled sans (Pen draws canvas overlays with it).
pub const INTER: &str = pen_layout::DEFAULT_SANS_FAMILY;

/// Font state shared by the windows.
#[derive(Clone, Default)]
pub struct AppFonts {
    /// Extra font files handed to every document's `TextEngine`. The bundled
    /// faces are already part of every `TextEngine`, so this stays empty
    /// unless user fonts are added.
    pub data: Arc<Vec<Arc<[u8]>>>,
    /// Inter was registered with GPUI.
    pub has_inter: bool,
}

impl Global for AppFonts {}

/// Bundled text faces as owned byte buffers for GPUI.
pub fn bundled() -> Vec<Cow<'static, [u8]>> {
    pen_layout::bundled_fonts()
        .into_iter()
        .map(|(_, data)| Cow::Borrowed(data))
        .collect()
}

/// Registers the bundled fonts with GPUI and stores the font global.
pub fn init(cx: &mut App) {
    // Scan the installed fonts off the UI thread now, so opening a document
    // that names a system family does not wait for it.
    pen_layout::prewarm_system_fonts();
    let fonts = bundled();
    let has_inter = !fonts.is_empty()
        && match cx.text_system().add_fonts(fonts) {
            Ok(()) => true,
            Err(e) => {
                eprintln!("openpen: could not register bundled fonts: {e:?}");
                false
            }
        };
    cx.set_global(AppFonts {
        data: Arc::new(Vec::new()),
        has_inter,
    });
}

pub fn fonts(cx: &App) -> AppFonts {
    cx.try_global::<AppFonts>().cloned().unwrap_or_default()
}

/// Family for canvas overlays (frame names, size labels, measurements):
/// Inter like Pen (docs/ui-spec.md §3).
pub fn overlay_family(cx: &App) -> &'static str {
    if fonts(cx).has_inter {
        INTER
    } else {
        "Segoe UI"
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn bundles_inter() {
        let fonts = pen_layout::bundled_fonts();
        assert!(!fonts.is_empty());
        assert!(
            fonts
                .iter()
                .any(|(name, _)| name.to_ascii_lowercase().contains("inter"))
        );
        assert_eq!(super::INTER, "Inter");
    }
}
