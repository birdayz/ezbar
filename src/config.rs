//! Configuration & theming (RFC 0002).
//!
//! `~/.config/ezbar/config.toml` drives bar placement, per-module options, and a
//! token theme. Everything is `#[serde(default)]`: a missing key falls back to a
//! built-in default, and **no file at all reproduces the current bar**. The parse
//! is pure ([`parse_str`]) so it is exhaustively unit-tested; [`load`] is the thin
//! I/O wrapper.

use std::collections::HashMap;
use std::path::PathBuf;

use ezbar_plugin::ThemeTokens;
use serde::Deserialize;

// ---------------------------------------------------------------------------
// Color — parsed from "#rgb", "#rrggbb", or "#rrggbbaa"
// ---------------------------------------------------------------------------

/// An RGBA color in 0..1, deserialized from a hex string.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Color(pub [f32; 4]);

impl Color {
    pub const fn rgba(r: f32, g: f32, b: f32, a: f32) -> Self {
        Color([r, g, b, a])
    }

    /// Parse `#rgb`, `#rrggbb`, or `#rrggbbaa` (with or without the leading `#`).
    pub fn parse(s: &str) -> Option<Color> {
        let h = s.strip_prefix('#').unwrap_or(s);
        let n = |i: usize, len: usize| -> Option<f32> {
            let part = h.get(i..i + len)?;
            let v = u8::from_str_radix(part, 16).ok()?;
            // expand a single nibble (e.g. "f" -> 0xff)
            let v = if len == 1 { v * 17 } else { v };
            Some(v as f32 / 255.0)
        };
        match h.len() {
            3 => Some(Color([n(0, 1)?, n(1, 1)?, n(2, 1)?, 1.0])),
            6 => Some(Color([n(0, 2)?, n(2, 2)?, n(4, 2)?, 1.0])),
            8 => Some(Color([n(0, 2)?, n(2, 2)?, n(4, 2)?, n(6, 2)?])),
            _ => None,
        }
    }

    pub fn iced(self) -> iced::Color {
        iced::Color::from_rgba(self.0[0], self.0[1], self.0[2], self.0[3])
    }
}

impl<'de> Deserialize<'de> for Color {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Color::parse(&s).ok_or_else(|| serde::de::Error::custom(format!("invalid color: {s:?}")))
    }
}

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Position {
    Top,
    #[default]
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Layer {
    Background,
    Bottom,
    #[default]
    Top,
    Overlay,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Style {
    #[default]
    Solid,
    Islands,
}

/// `"all"` or an explicit list of output names.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Outputs {
    Named(Vec<String>),
    Tag(String), // "all"
}

impl Default for Outputs {
    fn default() -> Self {
        Outputs::Tag("all".into())
    }
}

impl Outputs {
    pub fn matches(&self, name: &str) -> bool {
        match self {
            Outputs::Tag(_) => true, // "all"
            Outputs::Named(v) => v.iter().any(|n| n == name),
        }
    }
}

/// Corner radius: a single value or per-surface tiers.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(untagged)]
pub enum Radius {
    Uniform(f32),
    Tiered { item: f32, group: f32, popup: f32 },
}

impl Default for Radius {
    fn default() -> Self {
        Radius::Uniform(8.0)
    }
}

impl Radius {
    pub fn item(self) -> f32 {
        match self {
            Radius::Uniform(v) => v,
            Radius::Tiered { item, .. } => item,
        }
    }
    pub fn group(self) -> f32 {
        match self {
            Radius::Uniform(v) => v,
            Radius::Tiered { group, .. } => group,
        }
    }
    pub fn popup(self) -> f32 {
        match self {
            Radius::Uniform(v) => v,
            Radius::Tiered { popup, .. } => popup,
        }
    }
}

/// Tonal background: a flat color or `{ base, weak?, strong? }`.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Background {
    Solid(Color),
    Tonal {
        base: Color,
        weak: Option<Color>,
        strong: Option<Color>,
    },
}

impl Background {
    pub fn base(&self) -> Color {
        match self {
            Background::Solid(c) => *c,
            Background::Tonal { base, .. } => *base,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default)]
pub struct Border {
    pub width: f32,
    pub color: Color,
}

impl Default for Border {
    fn default() -> Self {
        Border {
            width: 0.0,
            color: Color::rgba(1.0, 1.0, 1.0, 0.08),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct PopupTheme {
    pub opacity: f32,
    pub backdrop: f32,
    pub radius: f32,
}

impl Default for PopupTheme {
    fn default() -> Self {
        // matches today's popup chrome: rgba(0,0,0,0.92), 8px corners
        PopupTheme {
            opacity: 0.92,
            backdrop: 0.0,
            radius: 8.0,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct WorkspaceTheme {
    pub focused: Color,
    pub occupied: Color,
    pub empty: Color,
    pub urgent: Color,
    pub colors: Vec<Color>,
    pub special: Vec<Color>,
}

impl Default for WorkspaceTheme {
    fn default() -> Self {
        WorkspaceTheme {
            focused: Color::rgba(1.0, 1.0, 1.0, 1.0),
            occupied: Color::rgba(0.55, 0.55, 0.55, 1.0),
            empty: Color::rgba(0.35, 0.35, 0.35, 1.0),
            urgent: Color::rgba(1.0, 0.2, 0.2, 1.0),
            colors: Vec::new(),
            special: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// [bar] / [theme]
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Bar {
    pub position: Position,
    pub layer: Layer,
    pub height: u32,
    pub outputs: Outputs,
    pub font: Option<String>,
    pub scale: f32,
}

impl Default for Bar {
    fn default() -> Self {
        Bar {
            position: Position::default(),
            layer: Layer::default(),
            height: 34,
            outputs: Outputs::default(),
            font: None,
            scale: 1.0,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Theme {
    pub style: Style,
    pub opacity: f32,
    pub font_size: f32,
    pub spacing: f32,
    pub padding: f32,
    pub radius: Radius,
    pub border: Border,
    pub background: Background,
    pub text: Color,
    pub dim: Color,
    pub primary: Color,
    pub ok: Color,
    pub warn: Color,
    pub urgent: Color,
    pub separator: Color,
    pub popup: PopupTheme,
    pub workspaces: WorkspaceTheme,
}

impl Default for Theme {
    /// Reproduces the current hardcoded bar so wiring config in is a no-op until
    /// the user actually writes a config.
    fn default() -> Self {
        Theme {
            style: Style::Solid,
            opacity: 0.8,
            font_size: 14.0,
            spacing: 6.0,
            padding: 6.0,
            radius: Radius::default(),
            border: Border::default(),
            background: Background::Solid(Color::rgba(0.0, 0.0, 0.0, 1.0)),
            text: Color::rgba(1.0, 1.0, 1.0, 1.0),
            dim: Color::rgba(0.7, 0.7, 0.7, 1.0),
            primary: Color::rgba(0.345, 0.65, 1.0, 1.0),
            ok: Color::rgba(0.2, 0.8, 0.2, 1.0),
            warn: Color::rgba(1.0, 0.67, 0.0, 1.0),
            urgent: Color::rgba(1.0, 0.2, 0.2, 1.0),
            separator: Color::rgba(0.4, 0.4, 0.4, 1.0),
            popup: PopupTheme::default(),
            workspaces: WorkspaceTheme::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Placement entries
// ---------------------------------------------------------------------------

/// One placement entry: a module id, a `{ id, key?, config? }` spec, or a group.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Entry {
    Group(Vec<Entry>),
    Spec(EntrySpec),
    Id(String),
}

fn empty_table() -> toml::Value {
    toml::Value::Table(toml::map::Map::new())
}

#[derive(Debug, Clone, Deserialize)]
pub struct EntrySpec {
    pub id: String,
    pub key: Option<String>,
    #[serde(default = "empty_table")]
    pub config: toml::Value,
}

impl Entry {
    /// The stable identity key for this entry (RFC 0002): explicit `key`, else `id`.
    /// Groups have no single key; `None`.
    pub fn key(&self) -> Option<&str> {
        match self {
            Entry::Id(id) => Some(id),
            Entry::Spec(s) => Some(s.key.as_deref().unwrap_or(&s.id)),
            Entry::Group(_) => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Top-level config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub bar: Bar,
    pub theme: Theme,
    pub left: Vec<Entry>,
    pub center: Vec<Entry>,
    pub right: Vec<Entry>,
    /// `[modules.<id>]` shared defaults, merged under each instance's inline config.
    pub modules: HashMap<String, toml::Value>,
}

impl Config {
    /// Resolve the `repr(C)` `ThemeTokens` handed to modules (RFC 0001).
    pub fn theme_tokens(&self) -> ThemeTokens {
        let t = &self.theme;
        ThemeTokens {
            fg: t.text.0,
            fg_dim: t.dim.0,
            urgent: t.urgent.0,
            warn: t.warn.0,
            ok: t.ok.0,
            accent: t.primary.0,
            sep: t.separator.0,
            text_size: t.font_size,
            bar_height: self.bar.height as u16,
        }
    }
}

/// `$XDG_CONFIG_HOME/ezbar/config.toml`, else `~/.config/ezbar/config.toml`.
pub fn path() -> Option<PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return Some(PathBuf::from(xdg).join("ezbar/config.toml"));
        }
    }
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(".config/ezbar/config.toml"))
}

/// Pure: parse a config from a TOML string.
pub fn parse_str(s: &str) -> Result<Config, String> {
    toml::from_str(s).map_err(|e| e.to_string())
}

/// Load the config; missing file or parse error falls back to defaults (logged).
pub fn load() -> Config {
    match path().and_then(|p| std::fs::read_to_string(p).ok()) {
        Some(s) => parse_str(&s).unwrap_or_else(|e| {
            log::warn!("config: {e}; using defaults");
            Config::default()
        }),
        None => Config::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_parses_hex_forms() {
        assert_eq!(Color::parse("#ffffff"), Some(Color([1.0, 1.0, 1.0, 1.0])));
        assert_eq!(Color::parse("000000"), Some(Color([0.0, 0.0, 0.0, 1.0])));
        assert_eq!(Color::parse("#fff"), Some(Color([1.0, 1.0, 1.0, 1.0])));
        let c = Color::parse("#58a6ff").unwrap();
        assert!((c.0[0] - 0x58 as f32 / 255.0).abs() < 1e-6);
        assert!((c.0[2] - 1.0).abs() < 1e-6);
        // alpha
        let a = Color::parse("#00000080").unwrap();
        assert!((a.0[3] - 128.0 / 255.0).abs() < 1e-6);
        assert_eq!(Color::parse("nope"), None);
        assert_eq!(Color::parse("#12"), None);
    }

    #[test]
    fn empty_config_is_defaults() {
        let c = parse_str("").unwrap();
        assert_eq!(c.bar.height, 34);
        assert_eq!(c.theme.style, Style::Solid);
        assert!(c.left.is_empty());
        // theme tokens reproduce the current bar
        let t = c.theme_tokens();
        assert_eq!(t.fg, [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(t.accent, [0.345, 0.65, 1.0, 1.0]);
        assert_eq!(t.bar_height, 34);
        assert_eq!(t.text_size, 14.0);
    }

    #[test]
    fn parses_a_real_config() {
        // NB: top-level keys (left/right) must precede any [table] header in TOML.
        let src = r##"
            left = ["workspaces", "window_title"]
            right = [["cpu", "memory"], { id = "stock", key = "nasdaq", config = { symbol = "NQ=F" } }, "clock"]

            [bar]
            position = "top"
            height = 30

            [theme]
            style = "islands"
            opacity = 0.95
            font_size = 13
            radius = { item = 4, group = 8, popup = 10 }
            primary = "#58a6ff"
            background = { base = "#0d1117", strong = "#21262d" }

            [modules.cpu]
            show_graph = true
        "##;
        let c = parse_str(src).unwrap();
        assert_eq!(c.bar.position, Position::Top);
        assert_eq!(c.bar.height, 30);
        assert_eq!(c.theme.style, Style::Islands);
        assert_eq!(c.theme.radius.group(), 8.0);
        assert_eq!(c.theme.radius.popup(), 10.0);
        assert_eq!(c.theme.primary, Color::parse("#58a6ff").unwrap());
        assert_eq!(c.theme.background.base(), Color::parse("#0d1117").unwrap());
        assert_eq!(c.theme_tokens().bar_height, 30);

        // placement: left = two ids
        assert_eq!(c.left.len(), 2);
        assert_eq!(c.left[0].key(), Some("workspaces"));

        // right[0] is a group of two
        match &c.right[0] {
            Entry::Group(g) => {
                assert_eq!(g.len(), 2);
                assert_eq!(g[0].key(), Some("cpu"));
            }
            other => panic!("expected group, got {other:?}"),
        }
        // right[1] is a spec with explicit key
        match &c.right[1] {
            Entry::Spec(s) => {
                assert_eq!(s.id, "stock");
                assert_eq!(s.key.as_deref(), Some("nasdaq"));
                assert_eq!(
                    s.config.get("symbol").and_then(|v| v.as_str()),
                    Some("NQ=F")
                );
            }
            other => panic!("expected spec, got {other:?}"),
        }
        assert_eq!(c.right[1].key(), Some("nasdaq"));

        // [modules.cpu]
        assert_eq!(
            c.modules
                .get("cpu")
                .and_then(|v| v.get("show_graph"))
                .and_then(|v| v.as_bool()),
            Some(true)
        );
    }

    #[test]
    fn radius_scalar_or_tiered() {
        let u: Config = parse_str("[theme]\nradius = 5").unwrap();
        assert_eq!(u.theme.radius.item(), 5.0);
        assert_eq!(u.theme.radius.popup(), 5.0);
    }

    #[test]
    fn bad_color_is_an_error_not_a_panic() {
        let err = parse_str("[theme]\nprimary = \"#xyz\"").unwrap_err();
        assert!(err.contains("invalid color"), "got: {err}");
    }

    #[test]
    fn outputs_all_vs_named() {
        let all = parse_str("[bar]\noutputs = \"all\"").unwrap();
        assert!(all.bar.outputs.matches("DP-1"));
        let named = parse_str("[bar]\noutputs = [\"DP-1\"]").unwrap();
        assert!(named.bar.outputs.matches("DP-1"));
        assert!(!named.bar.outputs.matches("eDP-1"));
    }
}
