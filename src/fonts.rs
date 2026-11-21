//! A fast font database.
//!
//! cosmic-text's default setup parses every installed font at startup
//! (~10 ms for ~1,200 faces) and then scores every face once per text style
//! (~3.5 ms each). doit instead:
//!
//! * caches the face list on disk, so startup reads one small file and
//!   memory-maps fonts only when they are used; the cache is rebuilt when any
//!   font directory changes;
//! * keeps every face of the theme's font, but only one representative face
//!   (closest to Regular) of every other family. Fallback can still reach
//!   every family, so no glyph is lost, but there are ~5× fewer faces to
//!   score.

use cosmic_text::FontSystem;
use fontdb::{Database, FaceInfo, Language, Source, Stretch, Style, Weight};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

const CACHE_VERSION: u32 = 1;

#[derive(Serialize, Deserialize, Clone)]
struct Face {
    path: PathBuf,
    index: u32,
    families: Vec<String>,
    post_script_name: String,
    /// 0 normal, 1 italic, 2 oblique
    style: u8,
    weight: u16,
    stretch: u16,
    monospaced: bool,
}

#[derive(Serialize, Deserialize)]
struct Cache {
    version: u32,
    /// Every font directory with its mtime: adding or removing a font changes one.
    dirs: Vec<(PathBuf, u64)>,
    faces: Vec<Face>,
}

fn cache_path() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::config::home().join(".cache"))
        .join("doit/fonts.json")
}

fn mtime(p: &Path) -> Option<u64> {
    let m = std::fs::metadata(p).ok()?.modified().ok()?;
    Some(m.duration_since(UNIX_EPOCH).ok()?.as_nanos() as u64)
}

/// Font directories: the usual roots and everything below them, plus the
/// folder of every known face (fontconfig may point elsewhere).
fn font_dirs(faces: &[Face]) -> Vec<(PathBuf, u64)> {
    let home = crate::config::home();
    let data = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".local/share"));
    let mut roots = vec![
        PathBuf::from("/usr/share/fonts"),
        PathBuf::from("/usr/local/share/fonts"),
        data.join("fonts"),
        home.join(".fonts"),
    ];
    roots.extend(faces.iter().filter_map(|f| f.path.parent().map(Path::to_path_buf)));
    let mut seen = std::collections::BTreeMap::new();
    let mut stack = roots;
    while let Some(d) = stack.pop() {
        if seen.contains_key(&d) {
            continue;
        }
        // Missing roots are recorded too (mtime 0): creating one invalidates.
        seen.insert(d.clone(), mtime(&d).unwrap_or(0));
        if let Ok(rd) = std::fs::read_dir(&d) {
            for e in rd.flatten() {
                if e.file_type().is_ok_and(|t| t.is_dir()) {
                    stack.push(e.path());
                }
            }
        }
    }
    seen.into_iter().collect()
}

fn load_cache() -> Option<Vec<Face>> {
    let bytes = std::fs::read(cache_path()).ok()?;
    let c: Cache = serde_json::from_slice(&bytes).ok()?;
    if c.version != CACHE_VERSION || c.dirs.iter().any(|(d, m)| mtime(d).unwrap_or(0) != *m) {
        return None;
    }
    Some(c.faces)
}

/// The slow path: parse every font, then remember what was found.
fn scan() -> Vec<Face> {
    let mut db = Database::new();
    db.load_system_fonts();
    let faces: Vec<Face> = db
        .faces()
        .filter_map(|f| {
            let path = match &f.source {
                Source::File(p) | Source::SharedFile(p, _) => p.clone(),
                Source::Binary(_) => return None,
            };
            Some(Face {
                path,
                index: f.index,
                families: f.families.iter().map(|(n, _)| n.clone()).collect(),
                post_script_name: f.post_script_name.clone(),
                style: match f.style {
                    Style::Normal => 0,
                    Style::Italic => 1,
                    Style::Oblique => 2,
                },
                weight: f.weight.0,
                stretch: f.stretch.to_number(),
                monospaced: f.monospaced,
            })
        })
        .collect();
    let cache = Cache { version: CACHE_VERSION, dirs: font_dirs(&faces), faces: faces.clone() };
    let path = cache_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_vec(&cache) {
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(tmp, path);
        }
    }
    faces
}

fn stretch(n: u16) -> Stretch {
    match n {
        1 => Stretch::UltraCondensed,
        2 => Stretch::ExtraCondensed,
        3 => Stretch::Condensed,
        4 => Stretch::SemiCondensed,
        6 => Stretch::SemiExpanded,
        7 => Stretch::Expanded,
        8 => Stretch::ExtraExpanded,
        9 => Stretch::UltraExpanded,
        _ => Stretch::Normal,
    }
}

/// How far a face is from plain Regular: the one to keep for fallback.
fn regularity(f: &Face) -> (u8, u16, u16) {
    (f.style, f.weight.abs_diff(400), f.stretch.abs_diff(5))
}

/// Families whose every face is kept: the theme's font and the generic
/// defaults cosmic-text resolves `sans-serif`, `monospace` and `serif` to.
const GENERIC: [&str; 3] = ["Open Sans", "Noto Sans Mono", "DejaVu Serif"];

fn select(faces: Vec<Face>, primary: &str) -> Vec<Face> {
    let primary = primary.to_lowercase();
    let keep_all = |f: &Face| {
        f.families.iter().any(|n| {
            let n = n.to_lowercase();
            n == primary || GENERIC.iter().any(|g| g.to_lowercase() == n)
        })
    };
    let mut out = vec![];
    let mut best: HashMap<String, Face> = HashMap::new();
    for f in faces {
        if keep_all(&f) {
            out.push(f);
            continue;
        }
        let Some(family) = f.families.first().cloned() else { continue };
        match best.get(&family) {
            Some(b) if regularity(b) <= regularity(&f) => {}
            _ => {
                best.insert(family, f);
            }
        }
    }
    out.extend(best.into_values());
    out
}

fn locale() -> String {
    let raw = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty() && v != "C" && v != "POSIX"))
        .unwrap_or_default();
    let tag = raw.split(['.', '@']).next().unwrap_or("").replace('_', "-");
    if tag.is_empty() { "en-US".into() } else { tag }
}

/// A FontSystem for `primary` (the theme font), built from the cache.
pub fn font_system(primary: &str) -> FontSystem {
    let faces = load_cache().unwrap_or_else(scan);
    let mut db = Database::new();
    let mut maps: HashMap<PathBuf, Option<Arc<memmap2::Mmap>>> = HashMap::new();
    for f in select(faces, primary) {
        let map = maps
            .entry(f.path.clone())
            .or_insert_with(|| {
                let file = std::fs::File::open(&f.path).ok()?;
                // SAFETY: font files are read-only system data, as fontdb itself assumes.
                unsafe { memmap2::Mmap::map(&file) }.ok().map(Arc::new)
            })
            .clone();
        let Some(map) = map else { continue };
        db.push_face_info(FaceInfo {
            id: fontdb::ID::dummy(),
            source: Source::SharedFile(f.path, map),
            index: f.index,
            families: f.families.into_iter().map(|n| (n, Language::English_UnitedStates)).collect(),
            post_script_name: f.post_script_name,
            style: match f.style {
                1 => Style::Italic,
                2 => Style::Oblique,
                _ => Style::Normal,
            },
            weight: Weight(f.weight),
            stretch: stretch(f.stretch),
            monospaced: f.monospaced,
        });
    }
    db.set_monospace_family(GENERIC[1]);
    db.set_sans_serif_family(GENERIC[0]);
    db.set_serif_family(GENERIC[2]);
    FontSystem::new_with_locale_and_db(locale(), db)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face(family: &str, weight: u16, style: u8) -> Face {
        Face {
            path: PathBuf::from(format!("/f/{family}-{weight}-{style}")),
            index: 0,
            families: vec![family.into()],
            post_script_name: String::new(),
            style,
            weight,
            stretch: 5,
            monospaced: false,
        }
    }

    #[test]
    fn keeps_primary_and_one_regular_per_family() {
        let faces = vec![
            face("Inter", 400, 0),
            face("Inter", 700, 0),
            face("Inter", 400, 1),
            face("Noto Sans Thai", 700, 0),
            face("Noto Sans Thai", 400, 1),
            face("Noto Sans Thai", 300, 0),
            face("Open Sans", 400, 0),
            face("Open Sans", 600, 0),
        ];
        let kept = select(faces, "inter");
        let names = |fam: &str| kept.iter().filter(|f| f.families[0] == fam).count();
        assert_eq!(names("Inter"), 3, "every face of the theme font");
        assert_eq!(names("Open Sans"), 2, "generic default families stay whole");
        let thai: Vec<_> = kept.iter().filter(|f| f.families[0] == "Noto Sans Thai").collect();
        assert_eq!(thai.len(), 1, "one face per other family");
        assert_eq!((thai[0].weight, thai[0].style), (300, 0), "upright, closest to Regular");
    }
}
