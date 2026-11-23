//! Font databases for the text engines (pen-layout and pen-raster), built so that opening a
//! document stays cheap:
//!
//! - [`bundled_db`]: a `fontdb::Database` with only the bundled text fonts (Inter). The faces
//!   are parsed once per process and cloned afterwards (no I/O, microseconds).
//! - [`catalog`]: the installed system fonts, scanned **once per process** (in parallel, on
//!   first use or ahead of time with [`prewarm`]). Engines copy faces from it into their own
//!   database **on demand**: [`load_family`] when a document names a family that is not
//!   bundled, [`load_fallbacks`] / [`load_all`] when a text has characters no loaded font
//!   covers.
//!
//! Why: cosmic-text's `FontSystem::get_font_matches` opens and parses every face of the
//! database whose weight differs from the requested one (to check a variable `wght` axis),
//! once per distinct (family, weight, style). With the ~400 system faces of a Windows install
//! as `Source::File`, that cost ~60 ms per combination (~550 ms for a real document). A small
//! database with in-memory sources makes the same query cost microseconds.

use fontdb::{Database, FaceInfo, Source};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

type Data = Arc<dyn AsRef<[u8]> + Send + Sync>;

/// Faces of the bundled text fonts, parsed once.
fn bundled_faces() -> &'static [FaceInfo] {
    static FACES: OnceLock<Vec<FaceInfo>> = OnceLock::new();
    FACES.get_or_init(|| {
        let mut db = Database::new();
        for f in crate::text_fonts() {
            db.load_font_source(Source::Binary(Arc::new(f.data)));
        }
        db.faces().cloned().collect()
    })
}

/// A database with only the bundled text fonts (Inter); sans-serif = Inter.
pub fn bundled_db() -> Database {
    let mut db = Database::new();
    for f in bundled_faces() {
        db.push_face_info(f.clone());
    }
    db.set_sans_serif_family(crate::DEFAULT_SANS);
    db
}

/// The installed system fonts (metadata only; the files are read when a family is loaded).
pub struct SystemFonts {
    faces: Vec<FaceInfo>,
    /// Lowercase family name (any language) -> indices into `faces`.
    by_family: HashMap<String, Vec<usize>>,
    /// File contents already read (shared by every engine).
    data: Mutex<HashMap<PathBuf, Option<Data>>>,
}

impl SystemFonts {
    /// Scans the installed fonts now (uncached; use [`catalog`] for the shared catalog).
    pub fn scan() -> Self {
        let faces = scan_system_faces();
        let mut by_family: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, f) in faces.iter().enumerate() {
            for (n, _) in &f.families {
                let v = by_family.entry(n.to_lowercase()).or_default();
                if v.last() != Some(&i) {
                    v.push(i);
                }
            }
        }
        Self {
            faces,
            by_family,
            data: Mutex::new(HashMap::new()),
        }
    }

    /// Number of system faces.
    pub fn len(&self) -> usize {
        self.faces.len()
    }

    pub fn is_empty(&self) -> bool {
        self.faces.is_empty()
    }

    /// True if an installed face has this family name (case-insensitive).
    pub fn has_family(&self, name: &str) -> bool {
        self.by_family.contains_key(&name.trim().to_lowercase())
    }

    fn file_data(&self, path: &Path) -> Option<Data> {
        let mut map = self.data.lock().unwrap_or_else(|p| p.into_inner());
        map.entry(path.to_path_buf())
            .or_insert_with(|| std::fs::read(path).ok().map(|v| Arc::new(v) as Data))
            .clone()
    }

    fn in_memory(&self, f: &FaceInfo) -> Option<FaceInfo> {
        let data = match &f.source {
            Source::File(p) => self.file_data(p)?,
            Source::SharedFile(_, d) | Source::Binary(d) => d.clone(),
        };
        let mut f = f.clone();
        f.source = Source::Binary(data);
        Some(f)
    }
}

/// The system font catalog (scans on first call; concurrent callers wait for the scan).
pub fn catalog() -> &'static SystemFonts {
    CATALOG.get_or_init(SystemFonts::scan)
}

static CATALOG: OnceLock<SystemFonts> = OnceLock::new();

/// Starts scanning the system fonts on a background thread (no-op if already scanned).
pub fn prewarm() {
    if CATALOG.get().is_none() {
        let _ = std::thread::Builder::new()
            .name("pen-fonts-scan".into())
            .spawn(|| {
                catalog();
            });
    }
}

/// True once the catalog has been scanned.
pub fn is_scanned() -> bool {
    CATALOG.get().is_some()
}

fn db_has_family(db: &Database, name: &str) -> Option<String> {
    let name = name.trim();
    db.faces().find_map(|f| {
        f.families
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(n, _)| n.clone())
    })
}

/// Canonical name of family `name` in `db`, loading its faces from the system catalog (into
/// memory) if `db` does not have it yet. `None` if the family is not installed.
pub fn load_family(db: &mut Database, name: &str) -> Option<String> {
    if let Some(n) = db_has_family(db, name) {
        return Some(n);
    }
    let cat = catalog();
    let idx = cat.by_family.get(&name.trim().to_lowercase())?;
    let mut added = false;
    for &i in idx {
        if let Some(f) = cat.in_memory(&cat.faces[i]) {
            db.push_face_info(f);
            added = true;
        }
    }
    if added { db_has_family(db, name) } else { None }
}

/// Loads every installed family of `names` that `db` lacks; returns how many were added.
pub fn load_families(db: &mut Database, names: &[&str]) -> usize {
    names
        .iter()
        .filter(|n| db_has_family(db, n).is_none() && load_family(db, n).is_some())
        .count()
}

/// Generic families resolved on demand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Generic {
    Serif,
    Monospace,
}

const SERIF: &[&str] = &[
    "Times New Roman",
    "Georgia",
    "Cambria",
    "Times",
    "Noto Serif",
    "DejaVu Serif",
    "Liberation Serif",
];

const MONOSPACE: &[&str] = &[
    "Cascadia Mono",
    "Consolas",
    "Courier New",
    "Menlo",
    "SF Mono",
    "DejaVu Sans Mono",
    "Liberation Mono",
    "Noto Sans Mono",
];

/// Makes `db`'s generic serif / monospace family point at an installed face (loading it).
/// Returns the family used.
pub fn load_generic(db: &mut Database, g: Generic) -> Option<String> {
    let list = match g {
        Generic::Serif => SERIF,
        Generic::Monospace => MONOSPACE,
    };
    let name = list.iter().find_map(|n| load_family(db, n))?;
    match g {
        Generic::Serif => db.set_serif_family(name.clone()),
        Generic::Monospace => db.set_monospace_family(name.clone()),
    }
    Some(name)
}

/// Adds every system face that `db` does not have yet, as lazy file sources (the text engine
/// maps a file when it first uses one of its faces). Used as the last resort for characters
/// no loaded font covers. Returns the number of faces added.
pub fn load_all(db: &mut Database) -> usize {
    let cat = catalog();
    let have: std::collections::HashSet<(String, u32)> = db
        .faces()
        .map(|f| (f.post_script_name.clone(), f.index))
        .collect();
    let mut n = 0;
    for f in &cat.faces {
        if !have.contains(&(f.post_script_name.clone(), f.index)) {
            db.push_face_info(f.clone());
            n += 1;
        }
    }
    n
}

/// Families tried first for characters the document's fonts do not cover.
#[cfg(target_os = "windows")]
const FALLBACK: &[&str] = &[
    "Segoe UI",
    "Segoe UI Emoji",
    "Segoe UI Symbol",
    "Segoe UI Historic",
    "Microsoft YaHei UI",
    "Yu Gothic UI",
    "Malgun Gothic",
    "Nirmala UI",
];
#[cfg(target_os = "macos")]
const FALLBACK: &[&str] = &[
    "Helvetica Neue",
    "Apple Color Emoji",
    "Apple Symbols",
    "PingFang SC",
    "Hiragino Sans",
    "Apple SD Gothic Neo",
];
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
const FALLBACK: &[&str] = &[
    "Noto Sans",
    "DejaVu Sans",
    "Noto Color Emoji",
    "Noto Sans CJK SC",
    "Noto Sans Symbols",
    "Noto Sans Symbols2",
];

/// How a family name resolved (see [`LazyFonts::resolve`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FamilyMatch {
    Named(String),
    SansSerif,
    Serif,
    Monospace,
}

/// On-demand loading policy shared by the text engines (pen-layout, pen-raster): which
/// system families / fallbacks were already copied into the engine's database.
#[derive(Debug, Clone, Default)]
pub struct LazyFonts {
    system: bool,
    /// Missing-glyph fallback stage: 0 none, 1 platform fallbacks, 2 every system face.
    stage: u8,
    /// Generic families already resolved (bit 0 serif, bit 1 monospace).
    generics: u8,
}

impl LazyFonts {
    /// Loads system families on demand.
    pub fn system() -> Self {
        Self {
            system: true,
            ..Self::default()
        }
    }

    /// Never touches the system fonts (the database is the caller's).
    pub fn none() -> Self {
        Self::default()
    }

    pub fn uses_system(&self) -> bool {
        self.system
    }

    /// True if the family is in `db` or (for system engines) installed.
    pub fn has_family(&self, db: &Database, name: &str) -> bool {
        db_has_family(db, name).is_some() || (self.system && catalog().has_family(name))
    }

    /// Resolves a CSS-like family name (generic keywords, case-insensitive names). Unknown
    /// families fall back to sans-serif. System families are loaded into `db` when needed.
    pub fn resolve(&mut self, db: &mut Database, name: &str) -> FamilyMatch {
        let lower = name.trim().to_ascii_lowercase();
        match lower.as_str() {
            "" | "sans-serif" | "system-ui" | "ui-sans-serif" => FamilyMatch::SansSerif,
            "serif" | "ui-serif" => {
                if self.system && self.generics & 1 == 0 {
                    self.generics |= 1;
                    load_generic(db, Generic::Serif);
                }
                FamilyMatch::Serif
            }
            "monospace" | "ui-monospace" => {
                if self.system && self.generics & 2 == 0 {
                    self.generics |= 2;
                    load_generic(db, Generic::Monospace);
                }
                FamilyMatch::Monospace
            }
            _ => {
                let found = match db_has_family(db, name) {
                    Some(n) => Some(n),
                    None if self.system => load_family(db, name),
                    None => None,
                };
                found.map_or(FamilyMatch::SansSerif, FamilyMatch::Named)
            }
        }
    }

    /// True while a missing glyph could still be fixed by [`LazyFonts::load_fallback`].
    pub fn can_fallback(&self) -> bool {
        self.system && self.stage < 2
    }

    /// Loads the next fallback stage (platform fallback families, then every system face).
    /// Returns false when nothing was added (the caller keeps its result).
    pub fn load_fallback(&mut self, db: &mut Database) -> bool {
        while self.can_fallback() {
            self.stage += 1;
            let added = if self.stage == 1 {
                load_families(db, FALLBACK)
            } else {
                load_all(db)
            };
            if added > 0 {
                return true;
            }
        }
        false
    }
}

/// True if `s` has a character that should get a glyph (not whitespace, control or a
/// default-ignorable format character). A `.notdef` glyph for such a cluster means no loaded
/// font covers it.
pub fn needs_glyph(s: &str) -> bool {
    s.chars().any(|c| {
        !c.is_whitespace()
            && !c.is_control()
            && !matches!(
                c,
                '\u{200b}'..='\u{200f}' | '\u{2060}' | '\u{fe00}'..='\u{fe0f}' | '\u{feff}'
            )
    })
}

/// Font directories scanned on this platform (`None` = let fontdb decide, e.g. fontconfig).
fn system_dirs() -> Option<Vec<PathBuf>> {
    #[cfg(target_os = "windows")]
    {
        let mut v = Vec::new();
        match std::env::var_os("SYSTEMROOT") {
            Some(r) => v.push(Path::new(&r).join("Fonts")),
            None => v.push(PathBuf::from("C:\\Windows\\Fonts")),
        }
        if let Some(home) = std::env::var_os("USERPROFILE") {
            let h = Path::new(&home);
            v.push(h.join("AppData\\Local\\Microsoft\\Windows\\Fonts"));
            v.push(h.join("AppData\\Roaming\\Microsoft\\Windows\\Fonts"));
        }
        Some(v)
    }
    #[cfg(target_os = "macos")]
    {
        let mut v = vec![
            PathBuf::from("/Library/Fonts"),
            PathBuf::from("/System/Library/Fonts"),
            PathBuf::from("/Network/Library/Fonts"),
        ];
        if let Ok(rd) = std::fs::read_dir("/System/Library/AssetsV2") {
            for e in rd.flatten() {
                if e.file_name()
                    .to_string_lossy()
                    .starts_with("com_apple_MobileAsset_Font")
                {
                    v.push(e.path());
                }
            }
        }
        if let Some(home) = std::env::var_os("HOME") {
            v.push(Path::new(&home).join("Library/Fonts"));
        }
        Some(v)
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        None
    }
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>, depth: u32) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let Ok(t) = e.file_type() else { continue };
        if t.is_dir() {
            if depth < 8 {
                collect_files(&p, out, depth + 1);
            }
        } else if p.extension().and_then(|x| x.to_str()).is_some_and(|x| {
            matches!(
                x.to_ascii_lowercase().as_str(),
                "ttf" | "otf" | "ttc" | "otc"
            )
        }) {
            out.push(p);
        }
    }
}

/// Parses the system font files in parallel; faces keep `Source::File`.
fn scan_system_faces() -> Vec<FaceInfo> {
    if std::env::var_os("PEN_NO_SYSTEM_FONTS").is_some() {
        return Vec::new();
    }
    let Some(dirs) = system_dirs() else {
        let mut db = Database::new();
        db.load_system_fonts();
        return db.faces().cloned().collect();
    };
    let mut files = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for d in dirs {
        let mut v = Vec::new();
        collect_files(&d, &mut v, 0);
        for p in v {
            if seen.insert(p.to_string_lossy().to_lowercase()) {
                files.push(p);
            }
        }
    }
    let threads = std::thread::available_parallelism()
        .map_or(4, |n| n.get())
        .clamp(1, 8);
    let per = files.len().div_ceil(threads).max(1);
    std::thread::scope(|s| {
        let handles: Vec<_> = files
            .chunks(per)
            .map(|chunk| {
                s.spawn(move || {
                    let mut db = Database::new();
                    for p in chunk {
                        let _ = db.load_font_file(p);
                    }
                    db.faces().cloned().collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap_or_default())
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_db_has_inter() {
        let db = bundled_db();
        assert_eq!(db.len(), crate::text_fonts().len());
        assert!(db_has_family(&db, "inter").is_some());
    }

    #[test]
    fn load_family_is_idempotent() {
        let mut db = bundled_db();
        let n = db.len();
        assert_eq!(load_family(&mut db, "INTER").as_deref(), Some("Inter"));
        assert_eq!(db.len(), n);
        assert_eq!(load_family(&mut db, "No Such Family 123"), None);
        if let Some(name) = ["Arial", "DejaVu Sans", "Helvetica"]
            .iter()
            .find(|f| catalog().has_family(f))
        {
            assert!(load_family(&mut db, name).is_some());
            let m = db.len();
            assert!(m > n);
            load_family(&mut db, name);
            assert_eq!(db.len(), m);
            // In-memory sources (no file access during matching).
            assert!(db.faces().all(|f| matches!(f.source, Source::Binary(_))));
        }
    }
}
