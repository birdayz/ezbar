//! Compile-in modules implementing `ezbar_plugin::Module` (RFC 0001, phase 1).

pub mod battery;
pub mod calendar;
pub mod claude;
pub mod clock;
pub mod cpu;
pub mod custom;
pub mod disk;
pub mod github;
pub mod ip;
pub mod keyboard;
pub mod kubectl;
pub mod memory;
pub mod net;
pub mod ping;
pub mod spotify;
pub mod stock;
pub mod temperature;
pub mod updates;
pub mod volume;
pub mod window_title;
pub mod workspaces;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use ezbar_plugin::Module;

/// Discovered WASM plugins, by placement id (RFC 0006). Populated once at startup.
static PLUGINS: OnceLock<HashMap<String, PathBuf>> = OnceLock::new();

/// Discover WASM plugins in `dir` and register them so their ids become placeable
/// like any built-in module. Call once at startup.
pub fn register_wasm_plugins(dir: &Path) {
    let map: HashMap<String, PathBuf> = ezbar_wasm::discover(dir).into_iter().collect();
    if !map.is_empty() {
        let mut ids: Vec<_> = map.keys().cloned().collect();
        ids.sort();
        log::info!("ezbar: {} wasm plugin(s): {ids:?}", map.len());
    }
    let _ = PLUGINS.set(map);
}

fn wasm_plugin_path(id: &str) -> Option<PathBuf> {
    PLUGINS.get().and_then(|m| m.get(id)).cloned()
}

/// Ids of all registered wasm plugins, sorted — for default placement injection.
pub fn wasm_plugin_ids() -> Vec<String> {
    let mut ids: Vec<String> = PLUGINS
        .get()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    ids.sort();
    ids
}

/// Granted network hosts for a plugin, from `[modules.<id>].network` (a string
/// or array of host names) — the capability the WASM tier enforces (RFC 0006 §5).
fn network_grants(cfg: &toml::Value) -> Vec<String> {
    string_or_array(cfg, "network")
}

/// Granted system-metric feeds for a plugin, from `[modules.<id>].feeds` (a string
/// or array of feed names: cpu/memory/temperature/battery/net) — RFC 0012's capability.
fn feed_grants(cfg: &toml::Value) -> Vec<String> {
    string_or_array(cfg, "feeds")
}

/// A `[modules.<id>].<key>` value that is either a single string or an array of strings.
fn string_or_array(cfg: &toml::Value, key: &str) -> Vec<String> {
    match cfg.get(key) {
        Some(toml::Value::String(s)) => vec![s.clone()],
        Some(toml::Value::Array(a)) => a
            .iter()
            .filter_map(|x| x.as_str().map(String::from))
            .collect(),
        _ => Vec::new(),
    }
}

fn flatten_cfg(cfg: &toml::Value) -> Vec<(String, String)> {
    cfg.as_table()
        .map(|t| {
            t.iter()
                .map(|(k, v)| {
                    let s = v
                        .as_str()
                        .map(String::from)
                        .unwrap_or_else(|| v.to_string());
                    (k.clone(), s)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Whether `id` names a built-in module or a registered wasm plugin (vs host
/// chrome such as `switcher`). Kept in sync with [`build`].
pub fn is_module(id: &str) -> bool {
    wasm_plugin_path(id).is_some()
        || matches!(
            id,
            "cpu"
                | "github"
                | "claude"
                | "custom"
                | "disk"
                | "net"
                | "ip"
                | "updates"
                | "keyboard"
                | "workspaces"
                | "memory"
                | "temperature"
                | "ping"
                | "window_title"
                | "clock"
                | "volume"
                | "battery"
                | "calendar"
                | "kubectl"
                | "stock"
                | "spotify"
        )
}

/// Read `[modules.<id>.graph].line_color` from a module's config table.
/// `None` (absent or not a string) means the default per-value threshold colouring
/// (green→red by load); a value is resolved against the theme by `Ctx::graph_paint`.
pub(crate) fn graph_line_color(cfg: &toml::Value) -> Option<String> {
    cfg.get("graph")?
        .get("line_color")?
        .as_str()
        .map(str::to_owned)
}

/// Build the sparkline canvas for a metric module from its resolved [`GraphCfg`] — the one
/// place the four near-identical metric views (cpu/memory/temperature/ping) share, so a graph
/// change (size, stroke, fill, a future `smooth`) touches one spot, not four. `line_color` is
/// the resolved override (`None` = per-value threshold colouring).
pub(crate) fn graph_widget<'a>(
    gcfg: &GraphCfg,
    kind: ezbar_plugin::ui::graph::GraphKind,
    values: Vec<f64>,
    line_color: Option<ezbar_plugin::iced::Color>,
) -> ezbar_plugin::iced::Element<'a, ezbar_plugin::ModMsg> {
    use ezbar_plugin::iced::{widget::canvas, Length};
    canvas(ezbar_plugin::ui::graph::Graph {
        values,
        kind,
        line_color,
        line_width: gcfg.line_width,
        fill: gcfg.fill,
    })
    .width(Length::Fixed(gcfg.width))
    .height(Length::Fixed(gcfg.height))
    .into()
}

/// Resolved `[modules.<id>.graph]` knobs for a metric module's sparkline (RFC 0002).
/// Every field has a sane default so an unconfigured graph looks exactly as before.
pub(crate) struct GraphCfg {
    pub samples: usize,  // history length (x-resolution)
    pub width: f32,      // canvas width px
    pub height: f32,     // canvas height px
    pub line_width: f32, // trace stroke px
    pub fill: bool,      // gradient area fill under the trace
    pub line_color: Option<String>,
}

/// Parse `[modules.<id>.graph]` into a [`GraphCfg`]. `default_samples` is the module's own
/// history default (cpu 30, memory 20, …) so an unset `samples` preserves current behaviour.
/// Numeric values are read as float-or-int and clamped to sane bounds (a fat-fingered
/// `line_width = 999` can't blow out the bar).
pub(crate) fn graph_cfg(cfg: &toml::Value, default_samples: usize) -> GraphCfg {
    let g = cfg.get("graph");
    let num = |k: &str, d: f32| {
        g.and_then(|g| g.get(k))
            .and_then(|v| {
                v.as_float()
                    .map(|f| f as f32)
                    .or_else(|| v.as_integer().map(|i| i as f32))
            })
            .unwrap_or(d)
    };
    let samples = g
        .and_then(|g| g.get("samples"))
        .and_then(|v| v.as_integer())
        .map(|i| i.clamp(2, 2048) as usize)
        .unwrap_or(default_samples);
    GraphCfg {
        samples,
        width: num("width", 48.0).clamp(8.0, 400.0),
        height: num("height", 16.0).clamp(6.0, 200.0),
        line_width: num("line_width", 1.5).clamp(0.5, 8.0),
        fill: g
            .and_then(|g| g.get("fill"))
            .and_then(|v| v.as_bool())
            .unwrap_or(true),
        line_color: graph_line_color(cfg),
    }
}

/// Construct a built-in module by its placement `id` (RFC 0001 factory). `cfg` is
/// the `[modules.<id>]` table. `rt` is the bar's runtime handle, used only by WASM
/// plugins (the reactor drives their tasks on it — RFC 0008). Returns `None` for ids
/// that are not modules.
pub fn build(
    id: &str,
    instance: u64,
    cfg: &toml::Value,
    rt: &tokio::runtime::Handle,
) -> Option<Box<dyn Module>> {
    match id {
        "cpu" => Some(Box::new(cpu::Cpu::new(instance, cfg))),
        "github" => Some(Box::new(github::GitHub::new(instance))),
        "claude" => Some(Box::new(claude::Claude::new(instance))),
        "custom" => Some(Box::new(custom::Custom::new(instance, id, cfg))),
        "disk" => Some(Box::new(disk::Disk::new(instance, cfg))),
        "net" => Some(Box::new(net::Net::new(instance, cfg))),
        "ip" => Some(Box::new(ip::Ip::new(instance, cfg))),
        "updates" => Some(Box::new(updates::Updates::new(instance, cfg))),
        "keyboard" => Some(Box::new(keyboard::Keyboard::new(instance, cfg))),
        "workspaces" => Some(Box::new(workspaces::Workspaces::new(instance, cfg))),
        "memory" => Some(Box::new(memory::Memory::new(instance, cfg))),
        "temperature" => Some(Box::new(temperature::Temperature::new(instance, cfg))),
        "ping" => Some(Box::new(ping::Ping::new(instance, cfg))),
        "window_title" => Some(Box::new(window_title::WindowTitle::new(instance, cfg))),
        "clock" => Some(Box::new(clock::Clock::new(instance, cfg))),
        "volume" => Some(Box::new(volume::Volume::new(instance))),
        "battery" => Some(Box::new(battery::Battery::new(instance))),
        "calendar" => Some(Box::new(calendar::Calendar::new(instance))),
        "kubectl" => Some(Box::new(kubectl::Kubectl::new(instance))),
        "stock" => Some(Box::new(stock::Stock::new(instance))),
        "spotify" => Some(Box::new(spotify::Spotify::new(instance))),
        // a registered WASM plugin (RFC 0006): load the `.wasm` as a Module.
        other => wasm_plugin_path(other).map(|path| {
            let m: Box<dyn Module> = Box::new(ezbar_wasm::WasmModule::new(
                rt.clone(),
                instance,
                other.to_string(),
                path,
                flatten_cfg(cfg),
                network_grants(cfg), // granted by `[modules.<id>].network`
                feed_grants(cfg),    // granted by `[modules.<id>].feeds` (RFC 0012)
                cfg.get("sway").and_then(|v| v.as_bool()).unwrap_or(false), // RFC 0013
            ));
            m
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tbl(s: &str) -> toml::Value {
        s.parse::<toml::Value>().unwrap()
    }

    #[test]
    fn graph_cfg_defaults_preserve_behaviour() {
        let g = graph_cfg(&tbl(""), 30);
        assert_eq!(g.samples, 30); // the module's own default is used when unset
        assert_eq!(g.width, 48.0);
        assert_eq!(g.height, 16.0);
        assert_eq!(g.line_width, 1.5);
        assert!(g.fill);
        assert!(g.line_color.is_none());
    }

    #[test]
    fn graph_cfg_parses_each_knob() {
        let g = graph_cfg(
            &tbl(
                "[graph]\nsamples = 80\nwidth = 64\nheight = 24\n\
                 line_width = 2.5\nfill = false\nline_color = \"accent\"",
            ),
            30,
        );
        assert_eq!(g.samples, 80);
        assert_eq!(g.width, 64.0);
        assert_eq!(g.height, 24.0);
        assert_eq!(g.line_width, 2.5);
        assert!(!g.fill);
        assert_eq!(g.line_color.as_deref(), Some("accent"));
    }

    #[test]
    fn graph_cfg_clamps_absurd_values() {
        // a fat-fingered config can't blow out the bar
        let c = graph_cfg(&tbl("[graph]\nline_width = 999\nheight = 0\nsamples = 1"), 30);
        assert_eq!(c.line_width, 8.0);
        assert_eq!(c.height, 6.0);
        assert_eq!(c.samples, 2);
    }
}
