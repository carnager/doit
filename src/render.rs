//! Software renderer: tiny-skia for shapes, cosmic-text for shaping/rasterizing.
//! All coordinates passed in are logical; we scale once here.

use crate::config::{Rgba, Theme};
use crate::icons::Icons;
use crate::state::{State, View};
use cosmic_text::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping, SwashCache, Weight, Wrap};
use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Rect, Stroke, Transform};

pub struct Layout {
    pub panel_w: f32,
    pub panel_h: f32,
    pub margin: f32,
    pub input_h: f32,
    pub list_y: f32,
    pub row_h: f32,
    pub row_step: f32,
    pub hints_h: f32,
}

impl Layout {
    pub fn new(t: &Theme) -> Layout {
        let input_h = (t.input_size * 1.25 + 30.0).round();
        let row_step = t.row_height + t.row_spacing;
        let list_h = if t.rows == 0 { 0.0 } else { t.rows as f32 * row_step - t.row_spacing + t.padding * 2.0 };
        let hints_h = if t.show_hints { 34.0 } else { 0.0 };
        let margin = t.shadow_size.max(0.0).ceil();
        Layout {
            panel_w: t.width as f32,
            panel_h: input_h + 1.0 + list_h + hints_h,
            margin,
            input_h,
            list_y: margin + input_h + 1.0 + t.padding,
            row_h: t.row_height,
            row_step,
            hints_h,
        }
    }
    pub fn surface_size(&self) -> (u32, u32) {
        ((self.panel_w + 2.0 * self.margin) as u32, (self.panel_h + 2.0 * self.margin) as u32)
    }
    /// Which list row (0-based, visible) is at logical (x, y)?
    pub fn row_at(&self, x: f64, y: f64, rows: usize) -> Option<usize> {
        let (x, y) = (x as f32, y as f32);
        if x < self.margin || x > self.margin + self.panel_w || y < self.list_y {
            return None;
        }
        let r = ((y - self.list_y) / self.row_step) as usize;
        let within = (y - self.list_y) - r as f32 * self.row_step <= self.row_h;
        (r < rows && within).then_some(r)
    }
    /// Grid geometry for `cols` columns inside the list area.
    /// Grid geometry for `cols` columns (0: as many ~150 px cells as fit)
    /// whose icons are `aspect` (h/w) tall.
    pub fn grid(&self, t: &Theme, cols: usize, aspect: f32) -> Grid {
        let area_w = self.panel_w - 2.0 * t.padding;
        let cols = if cols == 0 { ((area_w / 150.0) as usize).max(2) } else { cols };
        let area_h = t.rows as f32 * self.row_step - t.row_spacing;
        let cell_w = area_w / cols as f32;
        let label_h = t.font_size * 1.3 + 14.0;
        // As many rows as fit at the natural icon size, then stretch to fill.
        let natural = (cell_w - 20.0) * aspect;
        let rows = ((area_h / (natural + label_h + 12.0)).round() as usize).max(1);
        let cell_h = area_h / rows as f32;
        let icon_h = natural.min(cell_h - label_h - 12.0).max(16.0).floor();
        let icon = (icon_h / aspect).min(cell_w - 20.0).floor();
        Grid { cols, cell_w, cell_h, rows, icon, icon_h: (icon * aspect).floor(), label_h }
    }

    /// Index (relative to the first visible item) under logical (x, y).
    pub fn item_at(&self, t: &Theme, grid: Option<(usize, f32)>, rows: usize, x: f64, y: f64) -> Option<usize> {
        let Some((cols, aspect)) = grid else {
            return self.row_at(x, y, rows);
        };
        let g = self.grid(t, cols, aspect);
        let (x, y) = (x as f32 - self.margin - t.padding, y as f32 - self.list_y);
        if x < 0.0 || y < 0.0 {
            return None;
        }
        let (c, r) = ((x / g.cell_w) as usize, (y / g.cell_h) as usize);
        (c < g.cols && r < g.rows).then_some(r * g.cols + c)
    }

    pub fn inside_panel(&self, x: f64, y: f64) -> bool {
        let (x, y) = (x as f32, y as f32);
        x >= self.margin && x <= self.margin + self.panel_w && y >= self.margin && y <= self.margin + self.panel_h
    }
}

pub struct Grid {
    pub cols: usize,
    pub cell_w: f32,
    pub cell_h: f32,
    pub rows: usize,
    pub icon: f32,
    pub icon_h: f32,
    pub label_h: f32,
}

pub struct Renderer {
    pub fonts: FontSystem,
    cache: SwashCache,
    pub icons: Icons,
    chrome: Option<((u32, u32, u32), Pixmap)>,
}

struct Ctx<'a> {
    pm: &'a mut Pixmap,
    s: f32,
}

fn rrect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
    let mut pb = PathBuilder::new();
    if r <= 0.0 {
        pb.push_rect(Rect::from_xywh(x, y, w, h)?);
        return pb.finish();
    }
    // Squircle-ish corners (k slightly above the circular 0.552) look smoother.
    let k = 0.62 * r;
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.cubic_to(x + w - r + k, y, x + w, y + r - k, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.cubic_to(x + w, y + h - r + k, x + w - r + k, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.cubic_to(x + r - k, y + h, x, y + h - r + k, x, y + h - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    pb.close();
    pb.finish()
}

fn paint(c: Rgba) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(c.skia());
    p.anti_alias = true;
    p
}

impl Ctx<'_> {
    fn fill_rrect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, c: Rgba) {
        let s = self.s;
        if let Some(p) = rrect(x * s, y * s, w * s, h * s, r * s) {
            self.pm.fill_path(&p, &paint(c), FillRule::Winding, Transform::identity(), None);
        }
    }
    fn stroke_rrect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, width: f32, c: Rgba) {
        let s = self.s;
        let hw = width / 2.0;
        if let Some(p) = rrect((x + hw) * s, (y + hw) * s, (w - width) * s, (h - width) * s, (r - hw) * s) {
            let st = Stroke { width: width * s, ..Default::default() };
            self.pm.stroke_path(&p, &paint(c), &st, Transform::identity(), None);
        }
    }
}

/// Blend premultiplied-alpha text coverage into the pixmap with a clip & fade.
fn blend(pm: &mut Pixmap, x: i32, y: i32, c: cosmic_text::Color, clip: (i32, i32), fade_from: i32) {
    let (w, h) = (pm.width() as i32, pm.height() as i32);
    if x < 0 || y < 0 || x >= w || y >= h || x < clip.0 || x >= clip.1 {
        return;
    }
    let mut a = c.a() as f32 / 255.0;
    if x >= fade_from && clip.1 > fade_from {
        a *= 1.0 - (x - fade_from) as f32 / (clip.1 - fade_from) as f32;
    }
    if a <= 0.0 {
        return;
    }
    let i = (y * w + x) as usize * 4;
    let d = pm.data_mut();
    let inv = 1.0 - a;
    d[i] = (c.r() as f32 * a + d[i] as f32 * inv) as u8;
    d[i + 1] = (c.g() as f32 * a + d[i + 1] as f32 * inv) as u8;
    d[i + 2] = (c.b() as f32 * a + d[i + 2] as f32 * inv) as u8;
    d[i + 3] = (255.0 * a + d[i + 3] as f32 * inv) as u8;
}

pub struct TextSpec<'a> {
    pub text: &'a str,
    pub size: f32,
    pub weight: Weight,
    pub color: Rgba,
    pub hl: &'a [u32],
    pub hl_color: Rgba,
}

impl Renderer {
    pub fn new(fonts: FontSystem, icons: Icons) -> Renderer {
        Renderer { fonts, cache: SwashCache::new(), icons, chrome: None }
    }

    fn shape(&mut self, t: &TextSpec, family: &str, s: f32) -> Buffer {
        let fam = match family {
            "sans-serif" | "sans" => Family::SansSerif,
            "monospace" | "mono" => Family::Monospace,
            "serif" => Family::Serif,
            f => Family::Name(f),
        };
        let size = t.size * s;
        let mut buf = Buffer::new(&mut self.fonts, Metrics::new(size, (size * 1.3).ceil()));
        let mut b = buf.borrow_with(&mut self.fonts);
        b.set_wrap(Wrap::None);
        let base = Attrs::new().family(fam).weight(t.weight).color(t.color.cosmic());
        if t.hl.is_empty() {
            b.set_text(t.text, &base, Shaping::Advanced, None);
        } else {
            let hl = base.clone().color(t.hl_color.cosmic()).weight(Weight::SEMIBOLD);
            let mut spans: Vec<(&str, Attrs)> = vec![];
            let mut start = 0;
            let mut cur_hl = false;
            let mut hi = t.hl.iter().peekable();
            for (ci, (bi, _)) in t.text.char_indices().enumerate() {
                let is = hi.peek().is_some_and(|&&h| h as usize == ci);
                if is {
                    hi.next();
                }
                if is != cur_hl && bi > start {
                    spans.push((&t.text[start..bi], if cur_hl { hl.clone() } else { base.clone() }));
                    start = bi;
                }
                if is != cur_hl {
                    start = bi;
                }
                cur_hl = is;
            }
            spans.push((&t.text[start..], if cur_hl { hl.clone() } else { base.clone() }));
            b.set_rich_text(spans, &base, Shaping::Advanced, None);
        }
        b.shape_until_scroll(true);
        buf
    }

    fn width_of(buf: &Buffer) -> f32 {
        buf.layout_runs().map(|r| r.line_w).fold(0.0, f32::max)
    }

    /// Draw text with its vertical center at `cy`. Returns logical width.
    fn text(&mut self, pm: &mut Pixmap, family: &str, s: f32, t: &TextSpec, x: f32, cy: f32, max_w: f32) -> f32 {
        let mut buf = self.shape(t, family, s);
        let w = Self::width_of(&buf);
        let lh = buf.metrics().line_height;
        let ox = (x * s).round() as i32;
        let oy = (cy * s - lh / 2.0).round() as i32;
        let clip = (ox, ox + (max_w * s) as i32);
        let fade_from = if w > max_w * s { clip.1 - (28.0 * s) as i32 } else { i32::MAX };
        buf.draw(&mut self.fonts, &mut self.cache, t.color.cosmic(), |gx, gy, gw, gh, c| {
            for yy in 0..gh as i32 {
                for xx in 0..gw as i32 {
                    blend(pm, ox + gx + xx, oy + gy + yy, c, clip, fade_from);
                }
            }
        });
        w / s
    }

    fn measure(&mut self, family: &str, t: &TextSpec) -> f32 {
        Self::width_of(&self.shape(t, family, 1.0))
    }

    /// x offset (logical) of byte index `cursor` within `text`.
    fn caret_x(&mut self, family: &str, s: f32, t: &TextSpec, cursor: usize) -> f32 {
        let buf = self.shape(t, family, s);
        let Some(run) = buf.layout_runs().next() else { return 0.0 };
        run.glyphs.iter().find(|g| g.start >= cursor).map_or(run.line_w, |g| g.x) / s
    }

    /// Shadow + panel background + border, cached as one layer per size/scale.
    fn chrome(&mut self, pw: u32, ph: u32, scale: f32, t: &Theme, l: &Layout) -> Option<&Pixmap> {
        let key = (pw, ph, (scale * 100.0) as u32);
        if self.chrome.as_ref().map(|(k, _)| *k) != Some(key) {
            let mut pm = Pixmap::new(pw, ph)?;
            let c = &t.colors;
            if t.shadow_size > 0.0 && c.shadow.3 > 0 {
                shadow_layer(&mut pm, scale, t, l);
            }
            let mut ctx = Ctx { pm: &mut pm, s: scale };
            let (px, py) = (l.margin, l.margin);
            ctx.fill_rrect(px, py, l.panel_w, l.panel_h, t.radius, c.background);
            if t.border_width > 0.0 {
                ctx.stroke_rrect(px, py, l.panel_w, l.panel_h, t.radius, t.border_width, c.border);
            }
            self.chrome = Some((key, pm));
        }
        self.chrome.as_ref().map(|(_, p)| p)
    }

    /// Load icons for the visible rows in parallel (only first sighting is slow).
    fn prefetch_icons(&mut self, names: Vec<String>, w: u32, h: u32) {
        let missing: Vec<String> = names.into_iter().filter(|n| !self.icons.is_cached(n, w, h)).collect();
        if missing.len() < 2 {
            return;
        }
        let icons = &self.icons;
        let loaded: Vec<(String, Option<Pixmap>)> = std::thread::scope(|sc| {
            let hs: Vec<_> = missing
                .iter()
                .map(|n| sc.spawn(move || (n.clone(), icons.load_uncached(n, w, h))))
                .collect();
            hs.into_iter().filter_map(|h| h.join().ok()).collect()
        });
        for (n, p) in loaded {
            self.icons.insert(n, w, h, p);
        }
    }

    /// An icon, or a letter tile with a stable color per title.
    fn draw_icon(&mut self, pm: &mut Pixmap, fam: &str, s: f32, name: Option<&str>, title: &str, x: f32, y: f32, size: f32, h: f32, t: &Theme) {
        let (pw, ph) = ((size * s).round() as u32, (h * s).round() as u32);
        if let Some(ip) = name.and_then(|n| self.icons.get(n, pw, ph)).cloned() {
            pm.draw_pixmap((x * s).round() as i32, (y * s).round() as i32, ip.as_ref(), &Default::default(), Transform::identity(), None);
            return;
        }
        let hash = title.bytes().fold(5381u32, |a, b| a.wrapping_mul(33) ^ b as u32);
        let tile = hsl((hash % 360) as f32, 0.45, 0.55);
        Ctx { pm, s }.fill_rrect(x, y, size, h, size.min(h) * if size == h { 0.28 } else { 0.07 }, tile);
        let letter: String = title.chars().find(|c| c.is_alphanumeric()).unwrap_or('•').to_uppercase().collect();
        let spec = TextSpec { text: &letter, size: size * 0.5, weight: Weight::SEMIBOLD, color: Rgba(255, 255, 255, 240), hl: &[], hl_color: t.colors.text };
        let lw = self.measure(fam, &spec);
        self.text(pm, fam, s, &spec, x + (size - lw) / 2.0, y + h / 2.0, size);
    }

    /// Shape a sample in every weight we use so the first real frame is fast.
    pub fn warm_up(&mut self, t: &Theme) {
        let mut pm = match Pixmap::new(64, 64) {
            Some(p) => p,
            None => return,
        };
        for w in [Weight::NORMAL, Weight::MEDIUM, Weight::SEMIBOLD] {
            let spec = TextSpec { text: "Aa1…", size: t.font_size, weight: w, color: t.colors.text, hl: &[], hl_color: t.colors.text };
            self.text(&mut pm, &t.font.clone(), 1.0, &spec, 0.0, 10.0, 60.0);
        }
    }

    pub fn draw(&mut self, pm: &mut Pixmap, st: &mut State, t: &Theme, l: &Layout, s: f32) {
        let c = &t.colors;
        let fam = t.font.clone();
        let fam = fam.as_str();
        let (w, h) = (pm.width(), pm.height());
        match self.chrome(w, h, s, t, l) {
            Some(ch) => pm.data_mut().copy_from_slice(ch.data()),
            None => pm.fill(tiny_skia::Color::TRANSPARENT),
        }
        let (px, py) = (l.margin, l.margin);

        // ── input row ────────────────────────────────────────────────
        let pad = t.padding + 12.0;
        let icy = py + l.input_h / 2.0;
        let mut x = px + pad;
        {
            // magnifier glyph
            let r = t.input_size * 0.36;
            let (cx, cy) = (x + r, icy - r * 0.15);
            let mut pb = PathBuilder::new();
            pb.push_circle(cx * s, cy * s, r * s);
            let a = std::f32::consts::FRAC_1_SQRT_2;
            pb.move_to((cx + r * a) * s, (cy + r * a) * s);
            pb.line_to((cx + r * 1.75) * s, (cy + r * 1.75) * s);
            if let Some(p) = pb.finish() {
                let st = Stroke { width: 2.0 * s, line_cap: tiny_skia::LineCap::Round, ..Default::default() };
                pm.stroke_path(&p, &paint(c.subtext), &st, Transform::identity(), None);
            }
            x += r * 2.0 + 14.0;
        }

        // Geometry first: a provider may switch between list and grid.
        let grid = st.grid().map(|(cols, aspect)| {
            let g = l.grid(t, cols, aspect);
            (g.cols, g)
        });
        match &grid {
            Some((cols, g)) => st.set_geometry(g.rows, *cols),
            None => st.set_geometry(t.rows, 1),
        }

        let pill = match (&st.view, st.routed_source(), st.prompt()) {
            (View::Actions { item, .. }, _, _) => Some(format!("{} ›", st.items[*item].title)),
            _ if st.scope_label().is_some() => st.scope_label().map(str::to_string),
            (_, Some(src), p) => Some(p.unwrap_or(&src.name).to_string()),
            (_, _, Some(p)) => Some(p.to_string()),
            _ => None,
        };
        if let Some(p) = pill {
            let spec = TextSpec { text: &p, size: t.font_size - 1.0, weight: Weight::MEDIUM, color: c.accent, hl: &[], hl_color: c.accent };
            let w = self.measure(fam, &spec).min(220.0);
            let ph = t.input_size + 6.0;
            Ctx { pm, s }.fill_rrect(x, icy - ph / 2.0, w + 20.0, ph, ph / 2.0, c.accent.with_alpha(0.16));
            self.text(pm, fam, s, &spec, x + 10.0, icy, w);
            x += w + 30.0;
        }

        // counter on the right
        let mut right = px + l.panel_w - pad;
        if t.show_counter && st.view == View::List && t.rows > 0 {
            let n = format!("{}", st.hits.len());
            let spec = TextSpec { text: &n, size: t.subtext_size, weight: Weight::MEDIUM, color: c.hint, hl: &[], hl_color: c.hint };
            let w = self.measure(fam, &spec);
            self.text(pm, fam, s, &spec, right - w, icy, w + 2.0);
            right -= w + 14.0;
        }

        let in_actions = st.view != View::List;
        let pl = st.prefix_len();
        let masked: String;
        let qvis = if in_actions {
            ""
        } else if st.password {
            masked = "•".repeat(st.query[pl..].chars().count());
            &masked
        } else {
            &st.query[pl..]
        };
        // Byte offset of the cursor within what is shown.
        let vis_cursor = if st.password {
            st.query[pl..st.cursor.max(pl)].chars().count() * '•'.len_utf8()
        } else {
            st.cursor.saturating_sub(pl)
        };
        let max_w = (right - x).max(10.0);
        if qvis.is_empty() {
            let ph = if in_actions { "Choose an action" } else { st.placeholder() };
            let spec = TextSpec { text: ph, size: t.input_size, weight: Weight::NORMAL, color: c.placeholder, hl: &[], hl_color: c.placeholder };
            self.text(pm, fam, s, &spec, x + 6.0, icy, max_w);
        } else {
            let spec = TextSpec { text: qvis, size: t.input_size, weight: Weight::NORMAL, color: c.text, hl: &[], hl_color: c.text };
            self.text(pm, fam, s, &spec, x, icy, max_w);
        }
        if !in_actions {
            let spec = TextSpec { text: qvis, size: t.input_size, weight: Weight::NORMAL, color: c.text, hl: &[], hl_color: c.text };
            let cx = if qvis.is_empty() { 0.0 } else { self.caret_x(fam, s, &spec, vis_cursor) };
            let ch = t.input_size * 1.2;
            Ctx { pm, s }.fill_rrect(x + cx - 0.5, icy - ch / 2.0, 2.0, ch, 1.0, c.caret);
        }

        // separator
        if t.rows > 0 {
            Ctx { pm, s }.fill_rrect(px, py + l.input_h, l.panel_w, 1.0, 0.0, c.separator);
        }

        // ── rows ─────────────────────────────────────────────────────
        struct Row {
            title: String,
            subtitle: Option<String>,
            icon: Option<String>,
            badge: Option<String>,
            hl: Vec<u32>,
            selected: bool,
            hovered: bool,
            marked: bool,
            progress: Option<f32>,
        }
        let multi = st.sources.iter().filter(|s| s.prefix.is_none()).count() > 1;
        let mut rows: Vec<Row> = vec![];
        match &st.view {
            View::List => {
                let end = (st.scroll + st.page()).min(st.hits.len());
                for i in st.scroll..end {
                    let it = &st.items[st.hits[i].idx];
                    let (title, subtitle, icon) = (it.title.clone(), it.subtitle.clone(), it.icon.clone());
                    let src = st.sources.get(it.source);
                    let badge = it.badge.clone().or_else(|| {
                        (t.show_source && multi).then(|| src.map(|s| s.name.clone())).flatten()
                    });
                    let icon = icon.or_else(|| src.and_then(|s| s.icon.clone()));
                    let hl = st.ranker.indices(&title);
                    rows.push(Row {
                        title,
                        subtitle,
                        icon,
                        badge,
                        hl,
                        selected: i == st.selected,
                        hovered: st.hover == Some(i - st.scroll),
                        marked: st.marked.contains(&st.hits[i].idx),
                        progress: it.progress,
                    });
                }
            }
            View::Actions { item, selected } => {
                for (i, a) in st.actions_for(*item).into_iter().enumerate() {
                    rows.push(Row {
                        title: a.title,
                        subtitle: a.exec.clone(),
                        icon: a.icon,
                        badge: (i == 0).then(|| "↵".to_string()),
                        hl: vec![],
                        selected: i == *selected,
                        hovered: st.hover == Some(i),
                        marked: false,
                        progress: None,
                    });
                    if rows.len() >= st.rows {
                        break;
                    }
                }
            }
        }

        let (icon_w, icon_h) = grid.as_ref().map_or((t.icon_size, t.icon_size), |(_, g)| (g.icon, g.icon_h));
        if t.show_icons || grid.is_some() {
            let names: Vec<String> = rows.iter().filter_map(|r| r.icon.clone()).collect();
            self.prefetch_icons(names, (icon_w * s).round() as u32, (icon_h * s).round() as u32);
        }
        let rx = px + t.padding;
        let rw = l.panel_w - 2.0 * t.padding;
        if let Some((cols, g)) = &grid {
            for (k, row) in rows.iter().enumerate() {
                let x = rx + (k % cols) as f32 * g.cell_w;
                let y = l.list_y + (k / cols) as f32 * g.cell_h;
                if row.marked {
                    Ctx { pm, s }.stroke_rrect(x + 2.0, y + 2.0, g.cell_w - 4.0, g.cell_h - 4.0, t.row_radius, 2.0, c.accent);
                }
                if row.selected {
                    Ctx { pm, s }.fill_rrect(x + 2.0, y + 2.0, g.cell_w - 4.0, g.cell_h - 4.0, t.row_radius, c.selection);
                } else if row.hovered {
                    Ctx { pm, s }.fill_rrect(x + 2.0, y + 2.0, g.cell_w - 4.0, g.cell_h - 4.0, t.row_radius, c.selection.with_alpha(0.45));
                }
                let ix = x + (g.cell_w - g.icon) / 2.0;
                let iy = y + (g.cell_h - g.icon_h - g.label_h) / 2.0 + 2.0;
                self.draw_icon(pm, fam, s, row.icon.as_deref(), &row.title, ix, iy, g.icon, g.icon_h, t);
                if let Some(p) = row.progress {
                    progress_bar(pm, s, ix + 8.0, iy + g.icon_h - 10.0, g.icon - 16.0, 4.0, p, c.accent);
                }
                let tc = if row.selected { c.selection_text } else { c.text };
                let spec = TextSpec { text: &row.title, size: t.font_size - 1.0, weight: Weight::MEDIUM, color: tc, hl: &row.hl, hl_color: c.match_text };
                let maxw = g.cell_w - 16.0;
                let w = self.measure(fam, &spec).min(maxw);
                self.text(pm, fam, s, &spec, x + (g.cell_w - w) / 2.0, iy + g.icon_h + g.label_h / 2.0 + 2.0, maxw);
            }
        }
        for (ri, row) in rows.iter().enumerate().filter(|_| grid.is_none()) {
            let ry = l.list_y + ri as f32 * l.row_step;
            let cy = ry + l.row_h / 2.0;
            if row.selected {
                Ctx { pm, s }.fill_rrect(rx, ry, rw, l.row_h, t.row_radius, c.selection);
                if t.selection_bar {
                    let bh = l.row_h * 0.42;
                    Ctx { pm, s }.fill_rrect(rx + 1.0, cy - bh / 2.0, 3.0, bh, 1.5, c.accent);
                }
            } else if row.hovered {
                Ctx { pm, s }.fill_rrect(rx, ry, rw, l.row_h, t.row_radius, c.selection.with_alpha(0.45));
            }
            let mut tx = rx + 14.0;
            if t.show_icons {
                let is = t.icon_size;
                let ipx = (is * s).round() as u32;
                let iy = cy - is / 2.0;
                let _ = ipx;
                self.draw_icon(pm, fam, s, row.icon.as_deref(), &row.title, tx, iy, is, is, t);
                if let Some(p) = row.progress {
                    progress_bar(pm, s, tx + 2.0, iy + is - 5.0, is - 4.0, 3.0, p, c.accent);
                }
                tx += is + 14.0;
            }

            let mut tright = rx + rw - 14.0;
            if row.marked {
                let d = 18.0;
                Ctx { pm, s }.fill_rrect(tright - d, cy - d / 2.0, d, d, d / 2.0, c.accent);
                let spec = TextSpec { text: "✓", size: 11.5, weight: Weight::SEMIBOLD, color: c.background.with_alpha(1.0), hl: &[], hl_color: c.text };
                let w = self.measure(fam, &spec);
                self.text(pm, fam, s, &spec, tright - d / 2.0 - w / 2.0, cy, d);
                tright -= d + 10.0;
            }
            if let Some(b) = &row.badge {
                let spec = TextSpec { text: b, size: t.subtext_size - 0.5, weight: Weight::MEDIUM, color: c.badge_text, hl: &[], hl_color: c.badge_text };
                let w = self.measure(fam, &spec).min(140.0);
                let bh = t.subtext_size + 9.0;
                Ctx { pm, s }.fill_rrect(tright - w - 16.0, cy - bh / 2.0, w + 16.0, bh, 6.0, c.badge);
                self.text(pm, fam, s, &spec, tright - w - 8.0, cy, w);
                tright -= w + 28.0;
            }
            let tw = (tright - tx).max(10.0);
            let (tc, sc) = if row.selected { (c.selection_text, c.selection_subtext) } else { (c.text, c.subtext) };
            let title = TextSpec { text: &row.title, size: t.font_size, weight: Weight::MEDIUM, color: tc, hl: &row.hl, hl_color: c.match_text };
            match row.subtitle.as_deref().filter(|s| t.show_subtext && !s.is_empty()) {
                Some(sub) => {
                    let gap = 2.0;
                    let th = t.font_size * 1.3;
                    let sh = t.subtext_size * 1.3;
                    let top = cy - (th + sh + gap) / 2.0;
                    self.text(pm, fam, s, &title, tx, top + th / 2.0, tw);
                    let spec = TextSpec { text: sub, size: t.subtext_size, weight: Weight::NORMAL, color: sc, hl: &[], hl_color: sc };
                    self.text(pm, fam, s, &spec, tx, top + th + gap + sh / 2.0, tw);
                }
                None => {
                    self.text(pm, fam, s, &title, tx, cy, tw);
                }
            }
        }

        if rows.is_empty() && t.rows > 0 {
            let msg = if st.items.is_empty() { "Loading…" } else { "No matches" };
            let msg = if st.items.is_empty() && st.query.is_empty() { msg } else { "No matches" };
            let spec = TextSpec { text: msg, size: t.font_size, weight: Weight::NORMAL, color: c.placeholder, hl: &[], hl_color: c.placeholder };
            let w = self.measure(fam, &spec);
            let list_h = t.rows as f32 * l.row_step;
            self.text(pm, fam, s, &spec, px + (l.panel_w - w) / 2.0, l.list_y + list_h / 2.0 - t.padding, w + 2.0);
        }

        // ── hints ────────────────────────────────────────────────────
        if t.show_hints {
            let hy = py + l.panel_h - l.hints_h;
            Ctx { pm, s }.fill_rrect(px, hy, l.panel_w, 1.0, 0.0, c.separator);
            let cy = hy + l.hints_h / 2.0;
            let has_actions = match &st.view {
                View::List => st.hits.get(st.selected).is_some_and(|h| !st.items[h.idx].actions.is_empty()),
                View::Actions { .. } => true,
            };
            let enter_label = st.hits.get(st.selected).map(|h| st.actions_for(h.idx).swap_remove(0).title);
            let enter = if in_actions { "Run" } else { enter_label.as_deref().unwrap_or("Select") };
            let mut hints: Vec<(&str, &str)> = vec![("↵", enter)];
            if has_actions {
                hints.push(("⇥", if in_actions { "Back" } else { "Actions" }));
            }
            if st.password {
                hints = vec![("↵", "Submit")];
            }
            if st.switcher && !in_actions {
                hints = vec![("⇥", "Next"), ("↵", "Focus")];
            }
            if st.multi && !in_actions {
                hints.push(("⌃␣", if st.marked.is_empty() { "Mark" } else { "Mark · ↵ all" }));
            }
            if st.custom_allowed() && !in_actions && !st.password {
                hints.push(("⌃↵", "Use input"));
            }
            hints.push(("esc", "Close"));
            let mut hx = px + l.panel_w - pad;
            for (k, label) in hints.iter().rev() {
                let ls = TextSpec { text: label, size: t.subtext_size - 0.5, weight: Weight::NORMAL, color: c.hint, hl: &[], hl_color: c.hint };
                let ks = TextSpec { text: k, size: t.subtext_size - 1.5, weight: Weight::SEMIBOLD, color: c.subtext, hl: &[], hl_color: c.subtext };
                let lw = self.measure(fam, &ls);
                let kw = self.measure(fam, &ks).max(10.0);
                hx -= lw;
                self.text(pm, fam, s, &ls, hx, cy, lw + 2.0);
                hx -= kw + 12.0 + 6.0;
                let kh = t.subtext_size + 5.0;
                Ctx { pm, s }.fill_rrect(hx, cy - kh / 2.0, kw + 12.0, kh, 5.0, c.badge);
                self.text(pm, fam, s, &ks, hx + 6.0, cy, kw + 2.0);
                hx -= 18.0;
            }
            let left = st.message().map(str::to_string).or_else(|| {
                if !st.marked.is_empty() {
                    return Some(format!("{} marked", st.marked.len()));
                }
                st.routed_source().map(|s| s.name.clone()).or_else(|| (st.sources.len() == 1).then(|| st.sources[0].name.clone()))
            });
            if let Some(src) = left {
                let spec = TextSpec { text: &src, size: t.subtext_size - 0.5, weight: Weight::MEDIUM, color: c.hint, hl: &[], hl_color: c.hint };
                self.text(pm, fam, s, &spec, px + pad, cy, (hx - px - pad).max(0.0));
            }
        }
    }
}

/// A thin progress bar over an icon: dark track, accent fill.
fn progress_bar(pm: &mut Pixmap, s: f32, x: f32, y: f32, w: f32, h: f32, p: f32, accent: Rgba) {
    let p = p.clamp(0.0, 1.0);
    let mut c = Ctx { pm, s };
    c.fill_rrect(x - 1.0, y - 1.0, w + 2.0, h + 2.0, (h + 2.0) / 2.0, Rgba(0, 0, 0, 150));
    if p > 0.0 {
        c.fill_rrect(x, y, (w * p).max(h), h, h / 2.0, accent.with_alpha(1.0));
    }
}

fn hsl(h: f32, s: f32, l: f32) -> Rgba {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    let f = |v: f32| ((v + m) * 255.0).round() as u8;
    Rgba(f(r), f(g), f(b), 255)
}

/// Soft drop shadow: alpha mask at quarter resolution, 3× box blur (≈ gaussian),
/// bilinear upscale into the margin ring only (the panel covers the interior).
fn shadow_layer(pm: &mut Pixmap, scale: f32, t: &Theme, l: &Layout) {
    const DS: usize = 4;
    let (pw, ph) = (pm.width() as usize, pm.height() as usize);
    let (sw, sh) = (pw.div_ceil(DS), ph.div_ceil(DS));
    let Some(mut small) = Pixmap::new(sw as u32, sh as u32) else { return };
    let m = l.margin * scale / DS as f32;
    let off = m * 0.3;
    let mut cx = Ctx { pm: &mut small, s: 1.0 };
    let full = Rgba(0, 0, 0, 255);
    cx.fill_rrect(m, m + off, sw as f32 - 2.0 * m, sh as f32 - 2.0 * m, t.radius * scale / DS as f32, full);
    let mut a: Vec<u8> = small.data().as_chunks::<4>().0.iter().map(|p| p[3]).collect();
    let mut tmp = vec![0u8; a.len()];
    let r = (m * 0.42).max(1.0) as usize;
    for _ in 0..3 {
        blur_pass(&a, &mut tmp, sw, sh, r, true);
        blur_pass(&tmp, &mut a, sw, sh, r, false);
    }
    let sc = t.colors.shadow;
    // Interior rect that the opaque-ish panel fully covers.
    let inset = (l.margin + t.radius) * scale;
    let (ix0, iy0) = (inset as usize, inset as usize);
    let (ix1, iy1) = (pw.saturating_sub(inset as usize), ph.saturating_sub(inset as usize));
    let data = pm.data_mut();
    let k = 1.0 / DS as f32;
    for y in 0..ph {
        let fy = ((y as f32 + 0.5) * k - 0.5).max(0.0);
        let y0 = (fy as usize).min(sh - 1);
        let y1 = (y0 + 1).min(sh - 1);
        let ty = fy - y0 as f32;
        let inner_row = y >= iy0 && y < iy1;
        for x in 0..pw {
            if inner_row && x >= ix0 && x < ix1 {
                continue;
            }
            let fx = ((x as f32 + 0.5) * k - 0.5).max(0.0);
            let x0 = (fx as usize).min(sw - 1);
            let x1 = (x0 + 1).min(sw - 1);
            let tx = fx - x0 as f32;
            let s00 = a[y0 * sw + x0] as f32;
            let s10 = a[y0 * sw + x1] as f32;
            let s01 = a[y1 * sw + x0] as f32;
            let s11 = a[y1 * sw + x1] as f32;
            let v = (s00 * (1.0 - tx) + s10 * tx) * (1.0 - ty) + (s01 * (1.0 - tx) + s11 * tx) * ty;
            let alpha = v / 255.0 * sc.3 as f32 / 255.0;
            if alpha <= 0.002 {
                continue;
            }
            let i = (y * pw + x) * 4;
            data[i] = (sc.0 as f32 * alpha) as u8;
            data[i + 1] = (sc.1 as f32 * alpha) as u8;
            data[i + 2] = (sc.2 as f32 * alpha) as u8;
            data[i + 3] = (255.0 * alpha) as u8;
        }
    }
    // Cut the panel shape out so a translucent background stays uniform.
    if let Some(p) = rrect(l.margin * scale, l.margin * scale, l.panel_w * scale, l.panel_h * scale, t.radius * scale) {
        let mut paint = paint(Rgba(0, 0, 0, 255));
        paint.blend_mode = tiny_skia::BlendMode::Clear;
        pm.fill_path(&p, &paint, FillRule::Winding, Transform::identity(), None);
    }
}

fn blur_pass(src: &[u8], dst: &mut [u8], w: usize, h: usize, r: usize, horiz: bool) {
    let (outer, inner) = if horiz { (h, w) } else { (w, h) };
    let idx = |o: usize, i: usize| if horiz { o * w + i } else { i * w + o };
    let div = (2 * r + 1) as u32;
    for o in 0..outer {
        let mut acc = 0u32;
        for i in 0..=r.min(inner - 1) {
            acc += src[idx(o, i)] as u32;
        }
        for i in 0..inner {
            dst[idx(o, i)] = (acc / div) as u8;
            if i + r + 1 < inner {
                acc += src[idx(o, i + r + 1)] as u32;
            }
            if i >= r {
                acc -= src[idx(o, i - r)] as u32;
            }
        }
    }
}
