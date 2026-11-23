//! Bundled open-source fonts (files in `assets/fonts/`, see `assets/fonts/LICENSES.md`).
//!
//! - [`text_fonts`]: Inter (OFL-1.1), the default UI/design sans family of `.pen` documents.
//!   Every text backend must register these (pen-layout/pen-raster do it automatically; the GPUI
//!   app passes them to `TextSystem::add_fonts`).
//! - [`icon_fonts`]: icon fonts with a `name -> codepoint` map. They are consumed as glyph
//!   outlines by pen-layout, so backends do not need to register them.

pub mod system;

pub use fontdb;

/// A bundled font file.
#[derive(Debug, Clone, Copy)]
pub struct FontFile {
    /// File name (for diagnostics).
    pub file: &'static str,
    pub data: &'static [u8],
}

/// An icon font plus its `name -> codepoint` map.
#[derive(Debug, Clone, Copy)]
pub struct IconFont {
    /// Family names accepted in `iconFontFamily` (lowercase; the first is canonical).
    pub families: &'static [&'static str],
    pub font: FontFile,
    /// `"<name> <hex codepoint>\n"` lines.
    pub codepoints: &'static str,
    /// True if the font has a `wght` variation axis driven by `iconFontWeight`.
    pub variable_weight: bool,
}

macro_rules! font {
    ($path:literal) => {
        FontFile {
            file: $path,
            data: include_bytes!(concat!("../../../assets/fonts/", $path)),
        }
    };
}

static TEXT_FONTS: &[FontFile] = &[
    font!("inter/Inter-Light.ttf"),
    font!("inter/Inter-Regular.ttf"),
    font!("inter/Inter-Italic.ttf"),
    font!("inter/Inter-Medium.ttf"),
    font!("inter/Inter-SemiBold.ttf"),
    font!("inter/Inter-Bold.ttf"),
    font!("inter/Inter-ExtraBold.ttf"),
    font!("inter/Inter-Black.ttf"),
];

/// Family name of the bundled default sans font.
pub const DEFAULT_SANS: &str = "Inter";

/// Text fonts to register in every text system (Inter, 8 static faces).
pub fn text_fonts() -> &'static [FontFile] {
    TEXT_FONTS
}

#[cfg(feature = "material-symbols")]
const MATERIAL_CODEPOINTS: &str =
    include_str!("../../../assets/fonts/material-symbols/material-symbols.codepoints");

static ICON_FONTS: &[IconFont] = &[
    IconFont {
        families: &["lucide"],
        font: font!("lucide/lucide.ttf"),
        codepoints: include_str!("../../../assets/fonts/lucide/lucide.codepoints"),
        variable_weight: false,
    },
    #[cfg(feature = "material-symbols")]
    IconFont {
        families: &[
            "material symbols outlined",
            "material symbols",
            "material icons",
        ],
        font: font!("material-symbols/MaterialSymbolsOutlined.ttf"),
        codepoints: MATERIAL_CODEPOINTS,
        variable_weight: true,
    },
    #[cfg(feature = "material-symbols")]
    IconFont {
        families: &["material symbols rounded", "material icons round"],
        font: font!("material-symbols/MaterialSymbolsRounded.ttf"),
        codepoints: MATERIAL_CODEPOINTS,
        variable_weight: true,
    },
    #[cfg(feature = "material-symbols")]
    IconFont {
        families: &["material symbols sharp", "material icons sharp"],
        font: font!("material-symbols/MaterialSymbolsSharp.ttf"),
        codepoints: MATERIAL_CODEPOINTS,
        variable_weight: true,
    },
    #[cfg(feature = "phosphor")]
    IconFont {
        families: &["phosphor"],
        font: font!("phosphor/Phosphor.ttf"),
        codepoints: include_str!("../../../assets/fonts/phosphor/phosphor.codepoints"),
        variable_weight: false,
    },
];

/// Bundled icon fonts.
pub fn icon_fonts() -> &'static [IconFont] {
    ICON_FONTS
}

/// True if `data` is one of the bundled font files (text or icon font): text engines
/// already know the text fonts, and icon fonts are drawn as outlines.
pub fn is_bundled(data: &[u8]) -> bool {
    text_fonts()
        .iter()
        .chain(icon_fonts().iter().map(|i| &i.font))
        .any(|f| f.data.len() == data.len() && f.data == data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_are_truetype() {
        for f in text_fonts()
            .iter()
            .chain(icon_fonts().iter().map(|i| &i.font))
        {
            assert_eq!(
                &f.data[..4],
                &[0, 1, 0, 0],
                "{} is not a TrueType font",
                f.file
            );
        }
    }

    #[test]
    fn codepoint_maps_parse() {
        for i in icon_fonts() {
            let n = i
                .codepoints
                .lines()
                .map(|l| {
                    let (name, hex) = l.split_once(' ').unwrap();
                    assert!(!name.is_empty());
                    u32::from_str_radix(hex, 16).unwrap()
                })
                .count();
            assert!(n > 1000, "{}: {n} icons", i.families[0]);
        }
    }
}
