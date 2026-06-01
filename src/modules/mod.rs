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

use ezbar_plugin::Module;

/// Whether `id` names a built-in module (vs host chrome such as `switcher`).
/// Kept in sync with [`build`] — the single source of truth for "is this a module?".
pub fn is_module(id: &str) -> bool {
    matches!(
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
/// the `[modules.<id>]` table. Returns `None` for ids that are not modules.
pub fn build(id: &str, instance: u64, cfg: &toml::Value) -> Option<Box<dyn Module>> {
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
        _ => None,
    }
}
