//! Content-addressed asset files: `<dir>/images/<site>/<sha1[0:16]>.<ext>` and
//! `<dir>/fonts/<site>/...`, referenced from the `.pen` by relative paths.

use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Images,
    Fonts,
}

impl Kind {
    fn dir(self) -> &'static str {
        match self {
            Kind::Images => "images",
            Kind::Fonts => "fonts",
        }
    }
}

pub struct AssetWriter {
    dir: Option<PathBuf>,
    site: String,
    pub written: Vec<PathBuf>,
    known: HashMap<String, String>,
}

impl AssetWriter {
    pub fn new(dir: Option<PathBuf>, page_url: &str) -> Self {
        AssetWriter {
            dir,
            site: site_name(page_url),
            written: Vec::new(),
            known: HashMap::new(),
        }
    }

    pub fn enabled(&self) -> bool {
        self.dir.is_some()
    }

    /// Writes `bytes` (unless an identical file exists) and returns its relative path.
    pub fn store(&mut self, bytes: &[u8], kind: Kind, ext: &str) -> Result<String, String> {
        let Some(dir) = &self.dir else {
            return Err("no assets directory".into());
        };
        let hash = sha1_smol::Sha1::from(bytes).digest().to_string();
        let file = format!("{}.{ext}", &hash[..16]);
        let rel = format!("{}/{}/{file}", kind.dir(), self.site);
        if self.known.contains_key(&rel) {
            return Ok(rel);
        }
        let folder = dir.join(kind.dir()).join(&self.site);
        std::fs::create_dir_all(&folder)
            .map_err(|e| format!("cannot create {}: {e}", folder.display()))?;
        let path = folder.join(&file);
        if !path.exists() {
            std::fs::write(&path, bytes)
                .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
            self.written.push(path);
        }
        self.known.insert(rel.clone(), rel.clone());
        Ok(rel)
    }
}

/// Host name made safe for a folder name (`www.example.com`, `localhost-8080`).
pub fn site_name(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.rsplit('@').next().unwrap_or(host);
    let s: String = host
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let s = s.trim_matches(['-', '.']).to_string();
    if s.is_empty() { "page".into() } else { s }
}

/// Detected file type of downloaded bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sniff {
    Png,
    Jpeg,
    Webp,
    Gif,
    Svg,
    Avif,
    Heic,
    Ico,
    Bmp,
    Ttf,
    Otf,
    Woff,
    Woff2,
    Unknown,
}

impl Sniff {
    pub fn ext(self) -> &'static str {
        match self {
            Sniff::Png => "png",
            Sniff::Jpeg => "jpg",
            Sniff::Webp => "webp",
            Sniff::Gif => "gif",
            Sniff::Svg => "svg",
            Sniff::Avif => "avif",
            Sniff::Heic => "heic",
            Sniff::Ico => "ico",
            Sniff::Bmp => "bmp",
            Sniff::Ttf => "ttf",
            Sniff::Otf => "otf",
            Sniff::Woff => "woff",
            Sniff::Woff2 => "woff2",
            Sniff::Unknown => "bin",
        }
    }

    pub fn mime(self) -> &'static str {
        match self {
            Sniff::Png => "image/png",
            Sniff::Jpeg => "image/jpeg",
            Sniff::Webp => "image/webp",
            Sniff::Gif => "image/gif",
            Sniff::Svg => "image/svg+xml",
            Sniff::Avif => "image/avif",
            Sniff::Heic => "image/heic",
            Sniff::Ico => "image/x-icon",
            Sniff::Bmp => "image/bmp",
            _ => "application/octet-stream",
        }
    }

    /// Formats the OpenPen renderer decodes directly.
    pub fn renderable(self) -> bool {
        matches!(self, Sniff::Png | Sniff::Jpeg | Sniff::Webp)
    }
}

pub fn sniff(b: &[u8]) -> Sniff {
    let starts = |p: &[u8]| b.starts_with(p);
    if starts(b"\x89PNG") {
        Sniff::Png
    } else if starts(b"\xFF\xD8\xFF") {
        Sniff::Jpeg
    } else if b.len() > 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Sniff::Webp
    } else if starts(b"GIF8") {
        Sniff::Gif
    } else if b.len() > 12 && &b[4..8] == b"ftyp" {
        match &b[8..12] {
            b"avif" | b"avis" => Sniff::Avif,
            b"heic" | b"heix" | b"mif1" | b"msf1" | b"hevc" => Sniff::Heic,
            _ => Sniff::Unknown,
        }
    } else if starts(&[0, 0, 1, 0]) {
        Sniff::Ico
    } else if starts(b"BM") {
        Sniff::Bmp
    } else if starts(&[0, 1, 0, 0]) || starts(b"true") {
        Sniff::Ttf
    } else if starts(b"OTTO") {
        Sniff::Otf
    } else if starts(b"wOFF") {
        Sniff::Woff
    } else if starts(b"wOF2") {
        Sniff::Woff2
    } else {
        let head = String::from_utf8_lossy(&b[..b.len().min(512)]).to_ascii_lowercase();
        if head.contains("<svg") || (head.trim_start().starts_with("<?xml") && head.contains("svg"))
        {
            Sniff::Svg
        } else {
            Sniff::Unknown
        }
    }
}

/// Family name stored in a TTF/OTF file (typographic family first).
pub fn font_family(bytes: &[u8]) -> Option<String> {
    let face = ttf_parser::Face::parse(bytes, 0).ok()?;
    let names = face.names();
    let pick = |id: u16| {
        names
            .into_iter()
            .filter(|n| n.name_id == id && n.is_unicode())
            .find_map(|n| n.to_string())
    };
    pick(ttf_parser::name_id::TYPOGRAPHIC_FAMILY).or_else(|| pick(ttf_parser::name_id::FAMILY))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sites() {
        assert_eq!(site_name("https://www.Example.com/a?b"), "www.example.com");
        assert_eq!(site_name("http://127.0.0.1:8080/"), "127.0.0.1-8080");
        assert_eq!(site_name("about:blank"), "about-blank");
    }

    #[test]
    fn sniffing() {
        assert_eq!(sniff(b"\x89PNG\r\n"), Sniff::Png);
        assert_eq!(sniff(b"<svg xmlns='x'/>"), Sniff::Svg);
        assert_eq!(sniff(b"wOF2...."), Sniff::Woff2);
    }
}
