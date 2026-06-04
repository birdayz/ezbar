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

/// Where the `▾` preset switcher sits on the bar (RFC 0002).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SwitcherPos {
    Off,
    Left,
    #[default]
    Right,
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
            width: 1.0, // hairline on islands/popups; the flat bar draws none
            color: Color::rgba(1.0, 1.0, 1.0, 0.08),
        }
    }
}

/// How adjacent widgets *within a group* are divided (RFC 0005). Grouping itself
/// (sub-islands / wider gaps) carries the macro structure; this is the optional
/// per-widget mark on top of the spacing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SepStyle {
    /// pure spacing, no mark — the islands default (gaps between sub-islands do the
    /// separating; a glyph here is what made the bar read like a "CSV row").
    #[default]
    None,
    /// a dim middle dot `·`
    Dot,
    /// a thin vertical hairline
    Line,
    /// a custom glyph (`separator.glyph`, e.g. `"|"`)
    Glyph,
}

/// Separator between widgets in a zone: a bare hex color (⇒ a `line` of that color),
/// or `{ style?, color?, width?, glyph? }` (RFC 0005, extends RFC 0002's table).
#[derive(Debug, Clone, PartialEq)]
pub struct Separator {
    pub style: SepStyle,
    pub color: Color,
    /// line thickness / dot radius, in px.
    pub width: f32,
    pub glyph: Option<String>,
}

impl Default for Separator {
    fn default() -> Self {
        Separator {
            style: SepStyle::None,
            color: Color::rgba(0.4, 0.4, 0.4, 1.0),
            width: 1.0,
            glyph: None,
        }
    }
}

impl<'de> Deserialize<'de> for Separator {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Hex(String),
            Table {
                #[serde(default)]
                style: SepStyle,
                #[serde(default = "default_sep_color")]
                color: Color,
                #[serde(default = "default_sep_width")]
                width: f32,
                #[serde(default)]
                glyph: Option<String>,
            },
        }
        Ok(match Raw::deserialize(d)? {
            // a bare color string means "draw a hairline of this colour".
            Raw::Hex(s) => Separator {
                style: SepStyle::Line,
                color: Color::parse(&s)
                    .ok_or_else(|| serde::de::Error::custom(format!("invalid color: {s:?}")))?,
                ..Separator::default()
            },
            Raw::Table {
                style,
                color,
                width,
                glyph,
            } => Separator {
                // an explicit `glyph` with no `style` implies the glyph style.
                style: if style == SepStyle::None && glyph.is_some() {
                    SepStyle::Glyph
                } else {
                    style
                },
                color,
                width,
                glyph,
            },
        })
    }
}

fn default_sep_color() -> Color {
    Separator::default().color
}
fn default_sep_width() -> f32 {
    1.0
}

/// How the workspace indicator renders (RFC 0003). All four are square.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WsStyle {
    /// every workspace is a filled square cell (default)
    #[default]
    Boxed,
    /// only the focused workspace is filled; others are plain
    Filled,
    /// focused gets a square accent border, no fill
    Outlined,
    /// numbers with a 2px accent bar under the focused one
    Underbar,
}

impl WsStyle {
    /// Map to the internal chip-variant id used by the renderer.
    pub fn variant(self) -> u8 {
        match self {
            WsStyle::Filled => 1,
            WsStyle::Boxed => 2,
            WsStyle::Outlined => 3,
            WsStyle::Underbar => 4,
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
        // dark, square popups (radius 0) — ezbar's identity
        PopupTheme {
            opacity: 0.92,
            backdrop: 0.0,
            radius: 0.0,
        }
    }
}

/// `[theme.workspaces]` — only `style` lives here. The chip's colours come straight from the
/// global `[theme]` tokens (`accent`/`fg`/`fg_dim`/`urgent`), so the chip is themed by theming
/// the bar. (Earlier drafts carried per-state `focused`/`occupied`/`empty`/`urgent`/`colors`/
/// `special` fields that the renderer never read — config that lied; dropped, since the global
/// tokens already cover it. See RFC 0002 / TODO.)
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct WorkspaceTheme {
    pub style: WsStyle,
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
    /// font weight: thin|light|normal|medium|semibold|bold (RFC 0002)
    pub weight: Weight,
    pub scale: f32,
    pub switcher: SwitcherPos,
    /// gap from each screen edge — a non-zero margin floats the bar
    pub margin: Margin,
}

impl Default for Bar {
    fn default() -> Self {
        Bar {
            position: Position::default(),
            layer: Layer::default(),
            height: 34,
            outputs: Outputs::default(),
            font: None,
            weight: Weight::default(),
            scale: 1.0,
            switcher: SwitcherPos::default(),
            margin: Margin::default(),
        }
    }
}

/// Per-side gap from the screen edges (RFC 0002). Non-zero floats the bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(default)]
pub struct Margin {
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
    pub left: i32,
}

/// Font weight, mapped to `iced::font::Weight`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Weight {
    Thin,
    Light,
    #[default]
    Normal,
    Medium,
    Semibold,
    Bold,
}

impl Weight {
    pub fn iced(self) -> iced::font::Weight {
        use iced::font::Weight as W;
        match self {
            Weight::Thin => W::Thin,
            Weight::Light => W::Light,
            Weight::Normal => W::Normal,
            Weight::Medium => W::Medium,
            Weight::Semibold => W::Semibold,
            Weight::Bold => W::Bold,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Theme {
    pub style: Style,
    pub opacity: f32,
    pub font_size: f32,
    /// gap between widgets within a group (px).
    pub spacing: f32,
    /// gap between groups — the space between sub-islands (islands) or the gap a
    /// group divider sits in (solid). Wider than `spacing` so grouping reads (RFC 0005).
    pub group_gap: f32,
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
    pub separator: Separator,
    pub popup: PopupTheme,
    pub workspaces: WorkspaceTheme,
}

impl Default for Theme {
    /// ezbar's default identity: **lilac islands** — square floating panels over a
    /// dark, near-black base, with a flieder/lilac accent. Still square (radius is a
    /// hair, not a pill) and dark — deliberately not ashell's rounded islands.
    fn default() -> Self {
        let hex = |s: &str| Color::parse(s).expect("valid default hex");
        Theme {
            style: Style::Islands,
            opacity: 0.97,
            font_size: 14.0,
            spacing: 8.0,
            group_gap: 14.0,
            padding: 6.0,
            radius: Radius::Uniform(4.0), // near-square; islands need a hair of corner
            border: Border {
                width: 1.0,
                color: hex("#ffffff20"),
            },
            background: Background::Tonal {
                base: hex("#1e1e2e"),
                weak: Some(hex("#313244")),
                strong: Some(hex("#45475a")),
            },
            text: hex("#cdd6f4"),
            dim: hex("#a6adc8"),
            primary: hex("#cba6f7"), // flieder / lilac — the signature accent
            ok: hex("#a6e3a1"),
            warn: hex("#f9e2af"),
            urgent: hex("#f38ba8"),
            separator: Separator {
                style: SepStyle::None,
                color: hex("#585b70"),
                width: 1.0,
                glyph: None,
            },
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
            sep: t.separator.color.0,
            bg: t.background.base().0,
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

/// `…/ezbar/plugins/` — where WASM plugins (`*.wasm`) are dropped (RFC 0006).
pub fn plugins_dir() -> Option<PathBuf> {
    Some(path()?.with_file_name("plugins"))
}

/// Pure: parse a config from a TOML string (inline `[presets.*]` + `$palette` only;
/// no file I/O). [`parse_with`] adds drop-in `presets/*.toml` files.
pub fn parse_str(s: &str) -> Result<Config, String> {
    parse_with(s, &HashMap::new(), None)
}

/// Pure resolution pipeline (RFC 0002): apply the active **preset** (a theme bundle)
/// under the user's `[theme]`, resolve `$palette` references, then deserialize.
///
/// - `preset_files`: name → TOML body for drop-in `presets/<name>.toml`.
/// - `active`: the preset selected via the state file; overrides `[theme].preset`.
///
/// Precedence: built-in defaults < active preset < `[theme]` < per-module overrides.
pub fn parse_with(
    s: &str,
    preset_files: &HashMap<String, String>,
    active: Option<&str>,
) -> Result<Config, String> {
    let mut root: toml::Table = toml::from_str(s).map_err(|e| e.to_string())?;

    // 1. effective [theme] = preset theme (base) <- user [theme] (override)
    let user_theme = root.remove("theme");
    let preset_name = active.map(str::to_string).or_else(|| {
        user_theme
            .as_ref()
            .and_then(|t| t.get("preset"))
            .and_then(|v| v.as_str())
            .map(str::to_string)
    });

    let mut theme_tbl = toml::Table::new();
    if let Some(name) = &preset_name {
        if let Some(p) = load_preset_table(name, &root, preset_files)? {
            theme_tbl = p;
        }
    }
    if let Some(toml::Value::Table(ut)) = user_theme {
        merge_into(&mut theme_tbl, ut);
    }

    // 2. palette = top-level [palette] (base) <- preset/theme palette (wins)
    let mut palette = toml::Table::new();
    if let Some(toml::Value::Table(p)) = root.remove("palette") {
        merge_into(&mut palette, p);
    }
    if let Some(toml::Value::Table(p)) = theme_tbl.remove("palette") {
        merge_into(&mut palette, p);
    }

    // 3. resolve `$name` references against the palette (theme + per-module)
    resolve_refs_table(&mut theme_tbl, &palette)?;
    if let Some(toml::Value::Table(mods)) = root.get_mut("modules") {
        resolve_refs_table(mods, &palette)?;
    }

    theme_tbl.remove("preset"); // not a Theme field
    root.insert("theme".into(), toml::Value::Table(theme_tbl));

    // `presets`/`preset` left over are unknown to `Config` and ignored by serde.
    toml::Value::Table(root)
        .try_into()
        .map_err(|e: toml::de::Error| e.to_string())
}

/// Look up a preset by name: a `presets/<name>.toml` file (base) overlaid by an
/// inline `[presets.<name>]` table (wins). `None` if neither exists.
fn load_preset_table(
    name: &str,
    root: &toml::Table,
    files: &HashMap<String, String>,
) -> Result<Option<toml::Table>, String> {
    let mut t = toml::Table::new();
    let mut found = false;
    if let Some(src) = files.get(name) {
        let ft: toml::Table = toml::from_str(src).map_err(|e| format!("preset {name}: {e}"))?;
        merge_into(&mut t, ft);
        found = true;
    }
    if let Some(toml::Value::Table(presets)) = root.get("presets") {
        if let Some(toml::Value::Table(inline)) = presets.get(name) {
            merge_into(&mut t, inline.clone());
            found = true;
        }
    }
    Ok(found.then_some(t))
}

/// A module's effective config: `[modules.<id>]` defaults overlaid by the inline
/// `config` from a placement `{ id, key, config }` spec (inline wins).
pub fn merge_module_config(defaults: Option<&toml::Value>, inline: &toml::Value) -> toml::Value {
    let mut base = match defaults {
        Some(toml::Value::Table(t)) => t.clone(),
        _ => toml::Table::new(),
    };
    if let toml::Value::Table(i) = inline {
        merge_into(&mut base, i.clone());
    }
    toml::Value::Table(base)
}

/// Recursively merge `src` into `dst`: tables deep-merge, scalars/arrays overwrite.
fn merge_into(dst: &mut toml::Table, src: toml::Table) {
    for (k, sv) in src {
        let both_tables = matches!(dst.get(&k), Some(toml::Value::Table(_))) && sv.is_table();
        if both_tables {
            if let (Some(toml::Value::Table(dt)), toml::Value::Table(st)) = (dst.get_mut(&k), sv) {
                merge_into(dt, st);
            }
        } else {
            dst.insert(k, sv);
        }
    }
}

/// Replace every `"$name"` string in `tbl` with `palette[name]` (a hex string).
fn resolve_refs_table(tbl: &mut toml::Table, palette: &toml::Table) -> Result<(), String> {
    for (_k, v) in tbl.iter_mut() {
        resolve_refs_value(v, palette)?;
    }
    Ok(())
}

fn resolve_refs_value(v: &mut toml::Value, palette: &toml::Table) -> Result<(), String> {
    match v {
        toml::Value::String(s) => {
            if let Some(name) = s.strip_prefix('$') {
                // only treat `$word` as a ref; leave odd strings alone
                if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                    match palette.get(name) {
                        Some(toml::Value::String(hex)) => *s = hex.clone(),
                        Some(other) => {
                            return Err(format!("$palette ref ${name} is not a string: {other}"))
                        }
                        None => return Err(format!("unknown $palette ref: ${name}")),
                    }
                }
            }
        }
        toml::Value::Table(t) => resolve_refs_table(t, palette)?,
        toml::Value::Array(a) => {
            for e in a.iter_mut() {
                resolve_refs_value(e, palette)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// `<config-dir>/ezbar/presets/`.
fn presets_dir() -> Option<PathBuf> {
    path().and_then(|p| p.parent().map(|d| d.join("presets")))
}

/// Read every `presets/*.toml` into name → contents.
fn load_preset_files() -> HashMap<String, String> {
    let mut m = HashMap::new();
    if let Some(dir) = presets_dir() {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) != Some("toml") {
                    continue;
                }
                if let (Some(stem), Ok(src)) = (
                    p.file_stem().and_then(|s| s.to_str()),
                    std::fs::read_to_string(&p),
                ) {
                    m.insert(stem.to_string(), src);
                }
            }
        }
    }
    m
}

/// Sorted names of available drop-in presets (for the switcher).
pub fn preset_names() -> Vec<String> {
    let mut names: Vec<String> = load_preset_files().into_keys().collect();
    names.sort();
    names
}

/// `$XDG_STATE_HOME/ezbar/state.toml`, else `~/.local/state/ezbar/state.toml`.
fn state_path() -> Option<PathBuf> {
    if let Ok(x) = std::env::var("XDG_STATE_HOME") {
        if !x.is_empty() {
            return Some(PathBuf::from(x).join("ezbar/state.toml"));
        }
    }
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(".local/state/ezbar/state.toml"))
}

/// The preset the user last selected via the switcher (persisted, not in config).
pub fn active_preset() -> Option<String> {
    let s = std::fs::read_to_string(state_path()?).ok()?;
    let t: toml::Table = toml::from_str(&s).ok()?;
    t.get("preset").and_then(|v| v.as_str()).map(str::to_string)
}

/// Persist the active preset to the state file (never touches `config.toml`).
pub fn save_active_preset(name: &str) -> std::io::Result<()> {
    let p = state_path().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "no state path (HOME unset)")
    })?;
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(p, format!("preset = {name:?}\n"))
}

/// Run the full load pipeline (config.toml + drop-in presets + active-preset state),
/// returning `Err` on a parse/resolve failure so callers can keep the last-good
/// config. A missing `config.toml` is **not** an error — it resolves to defaults +
/// any active preset (so a preset selected via the switcher survives a file edit).
pub fn load_result() -> Result<Config, String> {
    let src = path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();
    let files = load_preset_files();
    let active = active_preset();
    parse_with(&src, &files, active.as_deref())
}

/// Load the config; missing file or parse error falls back to defaults (logged).
pub fn load() -> Config {
    load_result().unwrap_or_else(|e| {
        log::warn!("config: {e}; using defaults");
        Config::default()
    })
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
        // default identity: lilac islands
        assert_eq!(c.theme.style, Style::Islands);
        assert!(c.left.is_empty());
        // theme tokens reproduce the default bar (dark base, flieder/lilac accent)
        let t = c.theme_tokens();
        assert_eq!(t.fg, Color::parse("#cdd6f4").unwrap().0);
        assert_eq!(t.accent, Color::parse("#cba6f7").unwrap().0);
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

    #[test]
    fn palette_refs_resolve() {
        let src = r##"
            [palette]
            blue = "#58a6ff"
            base = "#0d1117"
            [theme]
            primary = "$blue"
            background = { base = "$base" }
            separator = { color = "$blue", width = 2 }
        "##;
        let c = parse_str(src).unwrap();
        assert_eq!(c.theme.primary, Color::parse("#58a6ff").unwrap());
        assert_eq!(c.theme.background.base(), Color::parse("#0d1117").unwrap());
        assert_eq!(c.theme.separator.color, Color::parse("#58a6ff").unwrap());
        assert_eq!(c.theme.separator.width, 2.0);
    }

    #[test]
    fn unknown_palette_ref_errors() {
        let err = parse_str("[theme]\nprimary = \"$nope\"").unwrap_err();
        assert!(err.contains("unknown $palette ref: $nope"), "got: {err}");
    }

    #[test]
    fn separator_accepts_hex_or_table() {
        // a bare hex ⇒ a hairline of that colour (RFC 0005).
        let hex = parse_str("[theme]\nseparator = \"#30363d\"").unwrap();
        assert_eq!(hex.theme.separator.color, Color::parse("#30363d").unwrap());
        assert_eq!(hex.theme.separator.glyph, None);
        assert_eq!(hex.theme.separator.style, SepStyle::Line);
        // a table with a glyph but no style ⇒ glyph style.
        let tbl = parse_str("[theme.separator]\nglyph = \"\"\nwidth = 1").unwrap();
        assert_eq!(tbl.theme.separator.glyph.as_deref(), Some(""));
        assert_eq!(tbl.theme.separator.style, SepStyle::Glyph);
    }

    #[test]
    fn separator_style_and_group_gap() {
        let c = parse_str("[theme.separator]\nstyle = \"dot\"").unwrap();
        assert_eq!(c.theme.separator.style, SepStyle::Dot);
        // defaults: no within-group mark, grouping carries the structure.
        assert_eq!(Config::default().theme.separator.style, SepStyle::None);
        assert_eq!(Config::default().theme.group_gap, 14.0);
        assert_eq!(Config::default().theme.spacing, 8.0);
        let g = parse_str("[theme]\ngroup_gap = 20").unwrap();
        assert_eq!(g.theme.group_gap, 20.0);
    }

    #[test]
    fn workspace_style_parses() {
        let c = parse_str("[theme.workspaces]\nstyle = \"outlined\"").unwrap();
        assert_eq!(c.theme.workspaces.style, WsStyle::Outlined);
        assert_eq!(c.theme.workspaces.style.variant(), 3);
        // default is boxed
        assert_eq!(Config::default().theme.workspaces.style, WsStyle::Boxed);
    }

    #[test]
    fn inline_preset_applies_under_user_theme() {
        // preset sets the base; the user's [theme] overrides one key on top.
        let src = r##"
            [theme]
            preset = "mine"
            primary = "#cccccc"     # overrides the preset

            [presets.mine]
            primary = "#aaaaaa"
            text = "#bbbbbb"
        "##;
        let c = parse_str(src).unwrap();
        assert_eq!(c.theme.primary, Color::parse("#cccccc").unwrap()); // user wins
        assert_eq!(c.theme.text, Color::parse("#bbbbbb").unwrap()); // from preset
    }

    #[test]
    fn shipped_presets_parse_and_resolve() {
        // Guard against the multi-line-inline-table bug: every shipped preset file
        // must parse AND fully resolve its $palette refs (no unresolved "$name").
        let presets: &[(&str, &str)] = &[
            ("ezbar-dark", include_str!("../presets/ezbar-dark.toml")),
            ("noir", include_str!("../presets/noir.toml")),
            ("nord", include_str!("../presets/nord.toml")),
            ("gruvbox-dark", include_str!("../presets/gruvbox-dark.toml")),
            (
                "catppuccin-mocha",
                include_str!("../presets/catppuccin-mocha.toml"),
            ),
            ("tokyo-night", include_str!("../presets/tokyo-night.toml")),
        ];
        for (name, body) in presets {
            let mut files = HashMap::new();
            files.insert(name.to_string(), body.to_string());
            let c = parse_with("", &files, Some(name))
                .unwrap_or_else(|e| panic!("preset {name} failed to load: {e}"));
            // the preset's accent must have resolved to a real opaque colour (a
            // leftover "$ref" would have errored above; a default fallback would be
            // caught by the per-preset spot-checks below)
            assert!(
                c.theme.primary.0[3] > 0.0,
                "preset {name}: primary did not resolve"
            );
        }
        // spot-check a couple of non-default presets actually applied their palette
        let mut nf = HashMap::new();
        nf.insert(
            "nord".to_string(),
            include_str!("../presets/nord.toml").into(),
        );
        let nord = parse_with("", &nf, Some("nord")).unwrap();
        assert_eq!(
            nord.theme.background.base(),
            Color::parse("#2e3440").unwrap()
        );
        assert_eq!(nord.theme.primary, Color::parse("#88c0d0").unwrap());
    }

    #[test]
    fn file_preset_via_parse_with_and_active_override() {
        let mut files = HashMap::new();
        files.insert(
            "nord".to_string(),
            "palette = { blue = \"#88c0d0\" }\nprimary = \"$blue\"\nstyle = \"islands\""
                .to_string(),
        );
        // active (state file) selects the preset even with an empty config
        let c = parse_with("", &files, Some("nord")).unwrap();
        assert_eq!(c.theme.primary, Color::parse("#88c0d0").unwrap());
        assert_eq!(c.theme.style, Style::Islands);
        // `active` overrides [theme].preset
        let c2 = parse_with("[theme]\npreset = \"other\"", &files, Some("nord")).unwrap();
        assert_eq!(c2.theme.primary, Color::parse("#88c0d0").unwrap());
    }
}
