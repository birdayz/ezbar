//! Compile-in modules implementing `ezbar_plugin::Module` (RFC 0001, phase 1).

pub mod claude;
pub mod cpu;
pub mod custom;
pub mod disk;
pub mod github;
pub mod ip;
pub mod keyboard;
pub mod net;
pub mod updates;

use ezbar_plugin::Module;

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
        _ => None,
    }
}
