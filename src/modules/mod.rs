//! Compile-in modules implementing `ezbar_plugin::Module` (RFC 0001, phase 1).

pub mod claude;
pub mod cpu;
pub mod custom;
pub mod disk;
pub mod github;
pub mod ip;
pub mod keyboard;
pub mod memory;
pub mod net;
pub mod ping;
pub mod temperature;
pub mod updates;
pub mod workspaces;

use ezbar_plugin::Module;

/// Whether `id` names a built-in module (vs a host-inline widget). Kept in sync with
/// [`build`] — the single source of truth for "is this a module?".
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
    )
}

/// Construct a built-in module by its placement `id` (RFC 0001 factory). `cfg` is
/// the `[modules.<id>]` table. Returns `None` for ids that aren't modules (those
/// are rendered inline by the host, e.g. `clock`, `volume`, `workspaces`).
pub fn build(id: &str, instance: u64, cfg: &toml::Value) -> Option<Box<dyn Module>> {
    match id {
        "cpu" => Some(Box::new(cpu::Cpu::new(instance))),
        "github" => Some(Box::new(github::GitHub::new(instance))),
        "claude" => Some(Box::new(claude::Claude::new(instance))),
        "custom" => Some(Box::new(custom::Custom::new(instance, id, cfg))),
        "disk" => Some(Box::new(disk::Disk::new(instance, cfg))),
        "net" => Some(Box::new(net::Net::new(instance, cfg))),
        "ip" => Some(Box::new(ip::Ip::new(instance, cfg))),
        "updates" => Some(Box::new(updates::Updates::new(instance, cfg))),
        "keyboard" => Some(Box::new(keyboard::Keyboard::new(instance, cfg))),
        "workspaces" => Some(Box::new(workspaces::Workspaces::new(instance, cfg))),
        "memory" => Some(Box::new(memory::Memory::new(instance))),
        "temperature" => Some(Box::new(temperature::Temperature::new(instance))),
        "ping" => Some(Box::new(ping::Ping::new(instance, cfg))),
        _ => None,
    }
}
