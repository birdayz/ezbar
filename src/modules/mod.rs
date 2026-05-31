//! Compile-in modules implementing `ezbar_plugin::Module` (RFC 0001, phase 1).

pub mod claude;
pub mod cpu;
pub mod custom;
pub mod github;

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
        _ => None,
    }
}
