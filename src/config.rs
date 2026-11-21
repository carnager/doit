//! Configuration and theming.
//!
//! Themes are layered TOML: a built-in base theme, optionally replaced by a
//! named theme file, then patched by inline `[theme]` overrides in the config.
//! Every field has a default, so partial themes are always valid.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const BUILTIN_THEMES: &[(&str, &str)] = &[
    ("midnight", include_str!("../themes/midnight.toml")),
    ("daylight", include_str!("../themes/daylight.toml")),
    ("nord", include_str!("../themes/nord.toml")),
    ("gruvbox", include_str!("../themes/gruvbox.toml")),
];

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgba(pub u8, pub u8, pub u8, pub u8);

impl Rgba {
    pub fn with_alpha(self, a: f32) -> Rgba {
        Rgba(self.0, self.1, self.2, (self.3 as f32 * a).round() as u8)
    }
    pub fn skia(self) -> tiny_skia::Color {
        tiny_skia::Color::from_rgba8(self.0, self.1, self.2, self.3)
    }
    pub fn cosmic(self) -> cosmic_text::Color {
        cosmic_text::Color::rgba(self.0, self.1, self.2, self.3)
    }
}

impl<'de> Deserialize<'de> for Rgba {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        parse_color(&s).ok_or_else(|| serde::de::Error::custom(format!("invalid color '{s}'")))
    }
}

impl Serialize for Rgba {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let Rgba(r, g, b, a) = *self;
        let hex = if a == 255 { format!("#{r:02x}{g:02x}{b:02x}") } else { format!("#{r:02x}{g:02x}{b:02x}{a:02x}") };
        s.serialize_str(&hex)
    }
}

pub fn dump_theme(t: &Theme) -> String {
    toml::to_string(t).unwrap_or_default()
}

/// Accepts `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa`.
pub fn parse_color(s: &str) -> Option<Rgba> {
    let h = s.trim().strip_prefix('#')?;
    let nib = |c: u8| (c as char).to_digit(16).map(|v| v as u8);
    let b = h.as_bytes();
    let byte = |i: usize| Some(nib(b[i])? << 4 | nib(b[i + 1])?);
    match b.len() {
        3 | 4 => {
            let v: Option<Vec<u8>> = b.iter().map(|&c| nib(c).map(|n| n << 4 | n)).collect();
            let v = v?;
            Some(Rgba(v[0], v[1], v[2], *v.get(3).unwrap_or(&255)))
        }
        6 => Some(Rgba(byte(0)?, byte(2)?, byte(4)?, 255)),
        8 => Some(Rgba(byte(0)?, byte(2)?, byte(4)?, byte(6)?)),
        _ => None,
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Colors {
    pub background: Rgba,
    pub border: Rgba,
    pub shadow: Rgba,
    pub text: Rgba,
    pub subtext: Rgba,
    pub placeholder: Rgba,
    pub accent: Rgba,
    pub caret: Rgba,
    pub selection: Rgba,
    pub selection_text: Rgba,
    pub selection_subtext: Rgba,
    pub match_text: Rgba,
    pub separator: Rgba,
    pub badge: Rgba,
    pub badge_text: Rgba,
    pub hint: Rgba,
}

impl Default for Colors {
    fn default() -> Self {
        // Mirrors themes/midnight.toml so a missing field never looks broken.
        let c = |s| parse_color(s).unwrap();
        Colors {
            background: c("#14151bf2"),
            border: c("#ffffff1a"),
            shadow: c("#00000080"),
            text: c("#e8e9f0"),
            subtext: c("#8b8ea3"),
            placeholder: c("#5d6075"),
            accent: c("#8b9cff"),
            caret: c("#8b9cff"),
            selection: c("#8b9cff26"),
            selection_text: c("#ffffff"),
            selection_subtext: c("#b4b9d6"),
            match_text: c("#a9b6ff"),
            separator: c("#ffffff12"),
            badge: c("#ffffff10"),
            badge_text: c("#8b8ea3"),
            hint: c("#5d6075"),
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Position {
    Center,
    Top,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Theme {
    pub font: String,
    pub font_size: f32,
    pub subtext_size: f32,
    pub input_size: f32,
    pub width: u32,
    pub rows: usize,
    pub row_height: f32,
    pub row_spacing: f32,
    pub radius: f32,
    pub row_radius: f32,
    pub padding: f32,
    pub icon_size: f32,
    pub border_width: f32,
    pub shadow_size: f32,
    pub position: Position,
    pub top_offset: f32,
    pub show_subtext: bool,
    pub show_icons: bool,
    pub show_hints: bool,
    pub show_counter: bool,
    pub show_source: bool,
    pub selection_bar: bool,
    pub icon_theme: Option<String>,
    pub colors: Colors,
}

impl Default for Theme {
    fn default() -> Self {
        Theme {
            font: "Inter".into(),
            font_size: 15.0,
            subtext_size: 12.5,
            input_size: 19.0,
            width: 680,
            rows: 8,
            row_height: 52.0,
            row_spacing: 2.0,
            radius: 20.0,
            row_radius: 12.0,
            padding: 10.0,
            icon_size: 32.0,
            border_width: 1.0,
            shadow_size: 28.0,
            position: Position::Center,
            top_offset: 0.22,
            show_subtext: true,
            show_icons: true,
            show_hints: true,
            show_counter: true,
            show_source: true,
            selection_bar: true,
            icon_theme: None,
            colors: Colors::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    pub name: String,
    /// Shell command; receives queries as JSON lines on stdin.
    pub exec: String,
    /// Only consulted when the query starts with this prefix (prefix is stripped).
    #[serde(default)]
    pub prefix: Option<String>,
    /// The provider filters results itself; doit shows them unfiltered, ranked first.
    #[serde(default)]
    pub live: bool,
    /// Include in the default mode set when no `-m` is given.
    #[serde(default)]
    pub default: bool,
    #[serde(default)]
    pub icon: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Built-in name or file name (without .toml) in ~/.config/doit/themes;
    /// "auto" follows the system's light/dark preference.
    pub theme_name: String,
    /// Used by "auto" when the system prefers light / dark.
    pub theme_light: String,
    pub theme_dark: String,
    pub modes: Vec<String>,
    pub terminal: Option<String>,
    /// Command prefix used to launch apps, e.g. ["niri", "msg", "action", "spawn", "--"].
    pub launch_prefix: Vec<String>,
    pub frecency: bool,
    #[serde(rename = "provider")]
    pub providers: Vec<ProviderConfig>,
    #[serde(skip)]
    pub theme: Theme,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            theme_name: "auto".into(),
            theme_light: "daylight".into(),
            theme_dark: "midnight".into(),
            modes: vec!["apps".into()],
            terminal: None,
            launch_prefix: vec![],
            frecency: true,
            providers: vec![],
            theme: Theme::default(),
        }
    }
}

/// The desktop's color-scheme preference (GNOME/GTK setting, which most
/// portals mirror). Anything but an explicit light preference counts as dark.
fn prefers_light() -> bool {
    if let Ok(v) = std::env::var("DOIT_COLOR_SCHEME") {
        return v == "light";
    }
    std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "color-scheme"])
        .stderr(std::process::Stdio::null())
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("prefer-light"))
}

pub fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"))
        .join("doit")
}

pub fn state_dir() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/state"))
        .join("doit")
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| "/".into())
}

pub fn expand_tilde(s: &str) -> String {
    match s.strip_prefix("~/") {
        Some(rest) => home().join(rest).to_string_lossy().into_owned(),
        None => s.to_string(),
    }
}

fn merge(base: &mut toml::Table, over: toml::Table) {
    for (k, v) in over {
        match (base.get_mut(&k), v) {
            (Some(toml::Value::Table(b)), toml::Value::Table(o)) => merge(b, o),
            (_, v) => {
                base.insert(k, v);
            }
        }
    }
}

pub fn load_theme_table(name: &str) -> Result<toml::Table> {
    let path = Path::new(name);
    let src = if path.extension().is_some_and(|e| e == "toml") && path.exists() {
        std::fs::read_to_string(path)?
    } else {
        let user = config_dir().join("themes").join(format!("{name}.toml"));
        if user.exists() {
            std::fs::read_to_string(&user).with_context(|| format!("reading {}", user.display()))?
        } else if let Some((_, s)) = BUILTIN_THEMES.iter().find(|(n, _)| *n == name) {
            s.to_string()
        } else {
            anyhow::bail!("unknown theme '{name}'");
        }
    };
    let t: toml::Table = toml::from_str(&src).with_context(|| format!("parsing theme '{name}'"))?;
    // Theme files may either be bare or wrapped in [theme].
    Ok(match t.get("theme") {
        Some(toml::Value::Table(inner)) => inner.clone(),
        _ => t,
    })
}

impl Config {
    pub fn load(theme_override: Option<&str>) -> Result<Config> {
        let path = config_dir().join("config.toml");
        let mut raw: toml::Table = if path.exists() {
            let s = std::fs::read_to_string(&path)?;
            toml::from_str(&s).with_context(|| format!("parsing {}", path.display()))?
        } else {
            toml::Table::new()
        };
        let inline_theme = match raw.remove("theme") {
            Some(toml::Value::Table(mut t)) => {
                // `[theme] name = "nord"` picks a base theme, other keys override it.
                if let Some(n) = t.remove("name") {
                    raw.insert("theme_name".into(), n);
                }
                Some(t)
            }
            Some(toml::Value::String(name)) => {
                raw.insert("theme_name".into(), toml::Value::String(name));
                None
            }
            _ => None,
        };
        let mut cfg: Config = raw.try_into().with_context(|| format!("in {}", path.display()))?;

        let mut name = theme_override.unwrap_or(&cfg.theme_name).to_string();
        if name == "auto" {
            name = if prefers_light() { cfg.theme_light.clone() } else { cfg.theme_dark.clone() };
        }
        let mut table = load_theme_table(&name)?;
        if theme_override.is_none()
            && let Some(over) = inline_theme
        {
            merge(&mut table, over);
        }
        cfg.theme = table.try_into().context("invalid theme")?;
        Ok(cfg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors() {
        assert_eq!(parse_color("#fff"), Some(Rgba(255, 255, 255, 255)));
        assert_eq!(parse_color("#11223344"), Some(Rgba(0x11, 0x22, 0x33, 0x44)));
        assert_eq!(parse_color("#1234"), Some(Rgba(0x11, 0x22, 0x33, 0x44)));
        assert_eq!(parse_color("123456"), None);
        assert_eq!(parse_color("#12345"), None);
    }

    #[test]
    fn builtin_themes_parse() {
        for (name, _) in BUILTIN_THEMES {
            let t: Theme = load_theme_table(name).unwrap().try_into().unwrap();
            assert!(t.width > 0, "{name}");
        }
    }
}
