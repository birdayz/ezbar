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
pub mod markup;
pub mod media;
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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{OnceLock, RwLock};

use ezbar_plugin::Module;

/// Global plugin **yolo** mode (`[plugins] yolo`). When set, every WASM plugin is built with
/// full capabilities, bypassing the per-module grants and the hash-consent. Set at config
/// load + reload by [`set_yolo`]; read in [`build`].
static PLUGIN_YOLO: AtomicBool = AtomicBool::new(false);

/// Apply `[plugins] yolo`. Logs loudly the first time it flips on — it's a security-relevant
/// mode (plugins get fs/network access). Idempotent; called on every config (re)load.
pub fn set_yolo(on: bool) {
    let was = PLUGIN_YOLO.swap(on, Ordering::Relaxed);
    if on && !was {
        log::warn!(
            "ezbar: [plugins] yolo=true — every WASM plugin now gets FULL capabilities \
             (read/write fs, any network host, all feeds, sway). They stay cpu/mem-sandboxed, \
             but trust your plugins. Turn it off for fine-grained, default-deny grants."
        );
    }
}

fn plugin_yolo() -> bool {
    PLUGIN_YOLO.load(Ordering::Relaxed)
}

/// The full-capability grant set for yolo mode: any host, any feed, sway, `/` read-write, and
/// any program. `"*"` wildcards are understood by the reactor's per-call checks; the fs grant
/// preopens the whole filesystem (still bounded by the OS user's own permissions).
fn yolo_grants() -> Grants {
    (
        vec!["*".to_string()],
        vec!["*".to_string()],
        true,
        vec![ezbar_wasm::FsGrant {
            host_path: PathBuf::from("/"),
            guest_path: "/".to_string(),
            write: true,
        }],
        vec!["*".to_string()],
    )
}

/// The five capability grant lists handed to a WASM plugin: network, feeds, sway, fs, exec.
type Grants = (
    Vec<String>,
    Vec<String>,
    bool,
    Vec<ezbar_wasm::FsGrant>,
    Vec<String>,
);

/// Discovered WASM plugins, by placement id (RFC 0006). Populated once at startup.
/// Discovered WASM plugins, by id. A re-settable `RwLock` (not a one-shot `OnceLock`) so a
/// `.wasm` dropped into the plugins dir is picked up on the next config reload, not only at
/// startup — [`register_wasm_plugins`] re-discovers, and reconcile rebuilds the module set.
static PLUGINS: OnceLock<RwLock<HashMap<String, PathBuf>>> = OnceLock::new();

fn plugins() -> &'static RwLock<HashMap<String, PathBuf>> {
    PLUGINS.get_or_init(|| RwLock::new(HashMap::new()))
}

/// (Re)discover WASM plugins in `dir` so their ids become placeable like any built-in. Called
/// at startup AND on every config reload, so dropping a new `.wasm` in is picked up live (the
/// following reconcile builds it). Logs only when the set actually changes (not every reload).
pub fn register_wasm_plugins(dir: &Path) {
    let map: HashMap<String, PathBuf> = ezbar_wasm::discover(dir).into_iter().collect();
    let changed = {
        let cur = plugins().read().unwrap_or_else(|e| e.into_inner());
        cur.len() != map.len() || map.keys().any(|k| !cur.contains_key(k))
    };
    if changed && !map.is_empty() {
        let mut ids: Vec<_> = map.keys().cloned().collect();
        ids.sort();
        log::info!("ezbar: {} wasm plugin(s): {ids:?}", map.len());
    }
    *plugins().write().unwrap_or_else(|e| e.into_inner()) = map;
}

fn wasm_plugin_path(id: &str) -> Option<PathBuf> {
    plugins().read().unwrap_or_else(|e| e.into_inner()).get(id).cloned()
}

/// Ids of all registered wasm plugins, sorted — for default placement injection.
pub fn wasm_plugin_ids() -> Vec<String> {
    let mut ids: Vec<String> = plugins().read().unwrap_or_else(|e| e.into_inner()).keys().cloned().collect();
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

/// Granted directories for a plugin, from `[modules.<id>].fs` — a list of
/// `{ path = "~/dir", mode = "r"|"rw", at = "/mount" }`. `path` is `~`-expanded; `mode`
/// defaults to read-only; `at` (the guest mount point) defaults to `/<basename>`. The WASM
/// tier preopens these into the guest's WASI filesystem (the fs capability tier).
/// Granted programs for a plugin, from `[modules.<id>].exec` (a string or array of program
/// names; `"*"` = any) — RFC 0015's exec capability tier. Only v0.3.0 plugins can call it.
fn exec_grants(cfg: &toml::Value) -> Vec<String> {
    string_or_array(cfg, "exec")
}

fn fs_grants(cfg: &toml::Value) -> Vec<ezbar_wasm::FsGrant> {
    let Some(arr) = cfg.get("fs").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|it| {
            let host_path = expand_tilde(it.get("path")?.as_str()?);
            let write = it
                .get("mode")
                .and_then(|v| v.as_str())
                .is_some_and(|m| m.eq_ignore_ascii_case("rw") || m.eq_ignore_ascii_case("w"));
            let guest_path = it
                .get("at")
                .and_then(|v| v.as_str())
                .map(String::from)
                .unwrap_or_else(|| {
                    let base = host_path.file_name().and_then(|s| s.to_str()).unwrap_or("dir");
                    format!("/{base}")
                });
            Some(ezbar_wasm::FsGrant { host_path, guest_path, write })
        })
        .collect()
}

/// Expand a leading `~/` against `$HOME`; leave everything else verbatim.
fn expand_tilde(p: &str) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => match std::env::var_os("HOME") {
            Some(home) => PathBuf::from(home).join(rest),
            None => PathBuf::from(p),
        },
        None => PathBuf::from(p),
    }
}

/// Warn for each capability a plugin's embedded `ezbar:manifest` *declares* but the user
/// did not *grant* in `[modules.<id>]` (RFC 0014 Phase A). The manifest is only a
/// declaration — enforcement is still the per-call host checks — so this never blocks a
/// load; it just turns a silently-inert widget into a logged, actionable diagnostic. A
/// plugin with no manifest (the common case today) produces nothing.
#[allow(clippy::too_many_arguments)]
fn warn_undeclared_grants(
    id: &str,
    path: &Path,
    net: &[String],
    feeds: &[String],
    sway: bool,
    fs: &[ezbar_wasm::FsGrant],
    exec: &[String],
) {
    let Some(m) = ezbar_wasm::manifest::read_file(path) else {
        return;
    };
    for h in &m.network {
        if !net.iter().any(|g| g == "*" || g.eq_ignore_ascii_case(h)) {
            log::warn!(
                "plugin '{id}' declares it needs network host {h:?}, but it isn't in \
                 [modules.{id}].network — requests there will be denied"
            );
        }
    }
    for f in &m.feeds {
        if !feeds.iter().any(|g| g == "*" || g == f) {
            log::warn!(
                "plugin '{id}' declares it needs feed {f:?}, but it isn't in \
                 [modules.{id}].feeds — it won't receive that metric"
            );
        }
    }
    if m.sway && !sway {
        log::warn!(
            "plugin '{id}' declares it needs sway, but [modules.{id}].sway isn't set — \
             sway-snapshot will be denied"
        );
    }
    // Dangerous tier (RFC 0015): declared fs/exec the user didn't grant. Loud — these are
    // the powerful ones, and a fetched plugin should never get them silently.
    if !m.fs.is_empty() && fs.is_empty() {
        log::warn!(
            "plugin '{id}' declares it needs filesystem access ({:?}), but [modules.{id}].fs \
             is unset — file reads will be denied (DANGEROUS tier; grant by hand)",
            m.fs
        );
    }
    for p in &m.exec {
        if !exec.iter().any(|g| g == "*" || g == p) {
            log::warn!(
                "plugin '{id}' declares it needs to run {p:?}, but it isn't in \
                 [modules.{id}].exec — exec will be denied (DANGEROUS tier; grant by hand)"
            );
        }
    }
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
                | "media"
                | "memory"
                | "temperature"
                | "ping"
                | "window_title"
                | "clock"
                | "volume"
                | "battery"
                | "calendar"
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
        "media" => Some(Box::new(media::Media::new(instance, cfg))),
        "memory" => Some(Box::new(memory::Memory::new(instance, cfg))),
        "temperature" => Some(Box::new(temperature::Temperature::new(instance, cfg))),
        "ping" => Some(Box::new(ping::Ping::new(instance, cfg))),
        "window_title" => Some(Box::new(window_title::WindowTitle::new(instance, cfg))),
        "clock" => Some(Box::new(clock::Clock::new(instance, cfg))),
        "volume" => Some(Box::new(volume::Volume::new(instance))),
        "battery" => Some(Box::new(battery::Battery::new(instance))),
        "calendar" => Some(Box::new(calendar::Calendar::new(instance))),
        "stock" => Some(Box::new(stock::Stock::new(instance))),
        "spotify" => Some(Box::new(spotify::Spotify::new(instance))),
        // a registered WASM plugin (RFC 0006): load the `.wasm` as a Module.
        other => wasm_plugin_path(other).map(|path| {
            // RFC 0014 Phase A: bind the capability grant to the artifact's *content hash*,
            // not its id. A binary the user never consented to (a same-named swap) inherits
            // nothing — it runs fully sandboxed until re-approved with `ezbar grant <id>`.
            let (net, feeds, sway, fs, exec) = if plugin_yolo() {
                // Yolo: full caps, no per-module grants, no hash-consent. (Still wasm-sandboxed.)
                yolo_grants()
            } else {
                match crate::grants::decide(other, &path) {
                crate::grants::Decision::Granted => {
                    let g: Grants = (
                        network_grants(cfg),                                       // `[modules.<id>].network`
                        feed_grants(cfg),                                          // `.feeds` (RFC 0012)
                        cfg.get("sway").and_then(|v| v.as_bool()).unwrap_or(false), // `.sway` (RFC 0013)
                        fs_grants(cfg),                                            // `.fs` (RFC 0015)
                        exec_grants(cfg),                                          // `.exec` (RFC 0015)
                    );
                    // RFC 0014 Phase A: if the plugin's embedded `ezbar:manifest` DECLARES a
                    // capability the user didn't grant, say so — an ungranted-and-therefore-
                    // silent widget then explains itself instead of failing mute.
                    warn_undeclared_grants(other, &path, &g.0, &g.1, g.2, &g.3, &g.4);
                    g
                }
                // The on-disk bytes don't match the consented hash — withhold every cap.
                crate::grants::Decision::Withheld => {
                    (Vec::new(), Vec::new(), false, Vec::new(), Vec::new())
                }
                }
            };
            let m: Box<dyn Module> = Box::new(ezbar_wasm::WasmModule::new(
                rt.clone(),
                instance,
                other.to_string(),
                path,
                flatten_cfg(cfg),
                net,
                feeds,
                sway,
                fs,
                exec,
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
    fn fs_grants_parse_path_mode_and_mount() {
        // explicit mount + rw
        let g = fs_grants(&tbl("fs = [{ path = \"/etc\", at = \"/etc-ro\", mode = \"rw\" }]"));
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].host_path, std::path::PathBuf::from("/etc"));
        assert_eq!(g[0].guest_path, "/etc-ro");
        assert!(g[0].write);
        // default mode = read-only, default mount = /<basename>
        let g = fs_grants(&tbl("fs = [{ path = \"/var/log\" }]"));
        assert_eq!(g[0].guest_path, "/log");
        assert!(!g[0].write);
        // no fs key → no grants (default-deny)
        assert!(fs_grants(&tbl("")).is_empty());
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
