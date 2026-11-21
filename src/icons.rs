//! Freedesktop icon theme lookup and rasterization (PNG + SVG), cached.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tiny_skia::Pixmap;

#[derive(Clone)]
struct Dir {
    path: String,
    size: u32,
    scale: u32,
    min: u32,
    max: u32,
    scalable: bool,
}

struct Theme {
    roots: Vec<PathBuf>,
    dirs: Vec<Dir>,
}

pub struct Icons {
    themes: Vec<Theme>,
    pixmaps: Vec<PathBuf>,
    cache: HashMap<(String, u32, u32), Option<Pixmap>>,
}

fn base_dirs() -> Vec<PathBuf> {
    let mut v = vec![crate::config::home().join(".icons")];
    for d in crate::providers::apps::data_dirs() {
        v.push(d.join("icons"));
    }
    v
}

fn parse_theme(name: &str, bases: &[PathBuf]) -> Option<(Theme, Vec<String>)> {
    let roots: Vec<PathBuf> = bases.iter().map(|b| b.join(name)).filter(|p| p.is_dir()).collect();
    let index = roots.iter().find_map(|r| std::fs::read_to_string(r.join("index.theme")).ok())?;
    let mut section = String::new();
    let mut inherits = vec![];
    let mut dir_names: Vec<String> = vec![];
    let mut props: HashMap<String, HashMap<String, String>> = HashMap::new();
    for line in index.lines() {
        let line = line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].to_string();
        } else if let Some((k, v)) = line.split_once('=') {
            let (k, v) = (k.trim(), v.trim());
            if section == "Icon Theme" {
                match k {
                    "Inherits" => inherits = v.split(',').map(|s| s.trim().to_string()).collect(),
                    "Directories" | "ScaledDirectories" => {
                        dir_names.extend(v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()))
                    }
                    _ => {}
                }
            } else {
                props.entry(section.clone()).or_default().insert(k.into(), v.into());
            }
        }
    }
    let dirs = dir_names
        .into_iter()
        .filter_map(|d| {
            let p = props.get(&d)?;
            let num = |k: &str| p.get(k).and_then(|v| v.parse::<u32>().ok());
            let size = num("Size")?;
            let ty = p.get("Type").map(|s| s.as_str()).unwrap_or("Threshold");
            let thr = num("Threshold").unwrap_or(2);
            let (min, max) = match ty {
                "Scalable" => (num("MinSize").unwrap_or(size), num("MaxSize").unwrap_or(size)),
                "Fixed" => (size, size),
                _ => (size.saturating_sub(thr), size + thr),
            };
            Some(Dir { path: d, size, scale: num("Scale").unwrap_or(1), min, max, scalable: ty == "Scalable" })
        })
        .collect();
    Some((Theme { roots, dirs }, inherits))
}

impl Icons {
    pub fn new(theme: Option<&str>) -> Icons {
        let bases = base_dirs();
        let mut chain: Vec<String> = vec![];
        let mut queue: Vec<String> = vec![theme.map(str::to_string).unwrap_or_else(detect_theme)];
        let mut themes = vec![];
        while let Some(t) = queue.pop() {
            if chain.contains(&t) {
                continue;
            }
            chain.push(t.clone());
            if let Some((th, inh)) = parse_theme(&t, &bases) {
                themes.push(th);
                for i in inh.into_iter().rev() {
                    queue.push(i);
                }
            }
        }
        if !chain.iter().any(|c| c == "hicolor")
            && let Some((th, _)) = parse_theme("hicolor", &bases)
        {
            themes.push(th);
        }
        let mut pixmaps = vec![];
        for d in crate::providers::apps::data_dirs() {
            pixmaps.push(d.join("pixmaps"));
        }
        Icons { themes, pixmaps, cache: HashMap::new() }
    }

    fn find(&self, name: &str, size: u32) -> Option<PathBuf> {
        if name.starts_with('/') {
            return Path::new(name).exists().then(|| PathBuf::from(name));
        }
        for th in &self.themes {
            let mut dirs: Vec<&Dir> = th.dirs.iter().collect();
            let dist = |d: &Dir| {
                let eff = size;
                let (mn, mx) = (d.min * d.scale, d.max * d.scale);
                if eff >= mn && eff <= mx {
                    0
                } else if eff < mn {
                    mn - eff
                } else {
                    // Upscaling bitmaps looks worse than downscaling.
                    (eff - mx) * 2
                }
            };
            dirs.sort_by_key(|d| (dist(d), !d.scalable, std::cmp::Reverse(d.size)));
            for d in dirs {
                for root in &th.roots {
                    for ext in ["svg", "png"] {
                        let p = root.join(&d.path).join(format!("{name}.{ext}"));
                        if p.exists() {
                            return Some(p);
                        }
                    }
                }
            }
        }
        for d in &self.pixmaps {
            for ext in ["svg", "png"] {
                let p = d.join(format!("{name}.{ext}"));
                if p.exists() {
                    return Some(p);
                }
            }
        }
        None
    }

    pub fn is_cached(&self, name: &str, w: u32, h: u32) -> bool {
        self.cache.contains_key(&(name.to_string(), w, h))
    }
    pub fn load_uncached(&self, name: &str, w: u32, h: u32) -> Option<Pixmap> {
        let path = self.find(name, w.max(h))?;
        // Image files given by path (covers, photos) are often huge; keep a
        // small rendered copy on disk so the next launch skips the decode.
        if !name.starts_with('/') {
            return load(&path, w, h);
        }
        let thumb = thumb_path(&path, w, h);
        if let Some(t) = &thumb
            && let Ok(pm) = Pixmap::load_png(t)
        {
            return Some(pm);
        }
        let pm = load(&path, w, h)?;
        if let Some(t) = thumb {
            if let Some(dir) = t.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let tmp = t.with_extension("tmp");
            if pm.save_png(&tmp).is_ok() {
                let _ = std::fs::rename(tmp, t);
            }
        }
        Some(pm)
    }
    pub fn insert(&mut self, name: String, w: u32, h: u32, pm: Option<Pixmap>) {
        self.cache.insert((name, w, h), pm);
    }

    /// A pixmap of exactly w×h px, or None if not found. Icons are fitted
    /// inside, photos (covers, posters) fill it.
    pub fn get(&mut self, name: &str, w: u32, h: u32) -> Option<&Pixmap> {
        let key = (name.to_string(), w, h);
        if !self.cache.contains_key(&key) {
            let pm = self.load_uncached(name, w, h);
            self.cache.insert(key.clone(), pm);
        }
        self.cache.get(&key).and_then(|o| o.as_ref())
    }
}

fn detect_theme() -> String {
    if let Ok(out) = std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "icon-theme"])
        .stderr(std::process::Stdio::null())
        .output()
    {
        let s = String::from_utf8_lossy(&out.stdout).trim().trim_matches('\'').to_string();
        if !s.is_empty() {
            return s;
        }
    }
    for f in [".config/gtk-4.0/settings.ini", ".config/gtk-3.0/settings.ini"] {
        if let Ok(s) = std::fs::read_to_string(crate::config::home().join(f)) {
            for l in s.lines() {
                if let Some(v) = l.trim().strip_prefix("gtk-icon-theme-name") {
                    let v = v.trim_start_matches([' ', '=']).trim().trim_matches('"');
                    if !v.is_empty() {
                        return v.to_string();
                    }
                }
            }
        }
    }
    "hicolor".into()
}

fn decode_jpeg(data: &[u8]) -> Option<Pixmap> {
    use zune_jpeg::zune_core::{colorspace::ColorSpace, options::DecoderOptions};
    let opts = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGBA);
    let mut dec = zune_jpeg::JpegDecoder::new_with_options(std::io::Cursor::new(data), opts);
    let rgba = dec.decode().ok()?;
    let (w, h) = dec.dimensions()?;
    // JPEG is opaque, so RGBA is already premultiplied.
    let size = tiny_skia::IntSize::from_wh(w as u32, h as u32)?;
    Pixmap::from_vec(rgba, size)
}

fn load(path: &Path, tw: u32, th: u32) -> Option<Pixmap> {
    let data = std::fs::read(path).ok()?;
    let is_svg = path.extension().is_some_and(|e| e == "svg" || e == "svgz");
    let is_photo = !is_svg && data.starts_with(&[0xFF, 0xD8]);
    let mut out = Pixmap::new(tw, th)?;
    let (twf, thf) = (tw as f32, th as f32);
    if is_svg {
        let tree = resvg::usvg::Tree::from_data(&data, &resvg::usvg::Options::default()).ok()?;
        let s = tree.size();
        let k = (twf / s.width()).min(thf / s.height());
        let dx = (twf - s.width() * k) / 2.0;
        let dy = (thf - s.height() * k) / 2.0;
        let t = tiny_skia::Transform::from_scale(k, k).post_translate(dx, dy);
        resvg::render(&tree, t, &mut out.as_mut());
    } else {
        let src = if data.starts_with(&[0xFF, 0xD8]) { decode_jpeg(&data)? } else { Pixmap::decode_png(&data).ok()? };
        // Icons fit inside the square; photos (covers, posters) fill it,
        // cropped with a bias to the top where faces and titles usually are.
        let (w, h) = (src.width() as f32, src.height() as f32);
        let k = if is_photo { (twf / w).max(thf / h) } else { (twf / w).min(thf / h) };
        let dx = (twf - w * k) / 2.0;
        let dy = (thf - h * k) * if is_photo { 0.2 } else { 0.5 };
        let paint = tiny_skia::PixmapPaint { quality: tiny_skia::FilterQuality::Bicubic, ..Default::default() };
        out.draw_pixmap(0, 0, src.as_ref(), &paint, tiny_skia::Transform::from_scale(k, k).post_translate(dx, dy), None);
        if is_photo {
            round_corners(&mut out, twf.min(thf) * if tw == th { 0.16 } else { 0.07 });
        }
    }
    Some(out)
}

/// Photos (album covers) get soft corners to sit nicely next to themed icons.
fn round_corners(pm: &mut Pixmap, r: f32) {
    let (w, h) = (pm.width() as f32, pm.height() as f32);
    let mut mask = match tiny_skia::Mask::new(pm.width(), pm.height()) {
        Some(m) => m,
        None => return,
    };
    let mut pb = tiny_skia::PathBuilder::new();
    let k = 0.62 * r;
    pb.move_to(r, 0.0);
    pb.line_to(w - r, 0.0);
    pb.cubic_to(w - r + k, 0.0, w, r - k, w, r);
    pb.line_to(w, h - r);
    pb.cubic_to(w, h - r + k, w - r + k, h, w - r, h);
    pb.line_to(r, h);
    pb.cubic_to(r - k, h, 0.0, h - r + k, 0.0, h - r);
    pb.line_to(0.0, r);
    pb.cubic_to(0.0, r - k, r - k, 0.0, r, 0.0);
    pb.close();
    if let Some(path) = pb.finish() {
        mask.fill_path(&path, tiny_skia::FillRule::Winding, true, tiny_skia::Transform::identity());
        pm.apply_mask(&mask);
    }
}

/// ~/.cache/doit/thumbs/<hash of path, mtime, size, px>.png
fn thumb_path(path: &Path, w: u32, h: u32) -> Option<PathBuf> {
    use std::hash::{Hash, Hasher};
    let md = std::fs::metadata(path).ok()?;
    let mtime = md.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (path, mtime, md.len(), w, h, 2u8).hash(&mut hasher);
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::config::home().join(".cache"));
    Some(cache.join("doit/thumbs").join(format!("{:016x}.png", hasher.finish())))
}
