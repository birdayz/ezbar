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
            ));
            m
        }),
    }
}
