use std::time::Duration;

use iced::alignment::{Horizontal, Vertical};
use iced::futures::{SinkExt, Stream};
use iced::widget::{button, column, container, mouse_area, row, scrollable, text, Space};
use iced::{
    event, window, Background, Border, Color, Element, Length, Padding, Subscription, Task,
};
use iced_layershell::build_pattern::daemon;
use iced_layershell::reexport::{
    Anchor, KeyboardInteractivity, Layer, NewLayerShellSettings, OutputOption,
};
use iced_layershell::settings::{LayerShellSettings, Settings, StartMode};
use iced_layershell::to_layer_message;

use ezbar::config::{self, Config, Style, SwitcherPos};
use ezbar::modules;
use ezbar::sources::volume;
use ezbar_plugin::{Ctx, HostRequest, ModMsg, Module, PopupMode, Reconfigure, ThemeTokens};

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

// jemalloc as the global allocator. glibc malloc hoards freed pages in per-thread
// arenas and only hands the top back via `malloc_trim`, so the bar's per-frame
// render churn sawtooths up to each 60s trim. jemalloc purges dirty pages to the OS
// continuously on a background thread, holding RSS at the working-set floor.
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

mod install;
mod ipc;
mod package;
mod registry;

/// Default right-zone placement when `right` is unconfigured — grouped into a few
/// semantic clusters that render as separate sub-islands (RFC 0005). The gaps between
/// groups are the separators; the order is `clock` last so time anchors the far edge.
const DEFAULT_RIGHT_GROUPS: &[&[&str]] = &[
    &["cpu", "memory", "temperature"],   // machine vitals
    &["ping", "github", "claude"],       // connectivity + dev
    &["calendar", "kubectl", "spotify"], // work + media
    &["stock", "volume", "battery"],     // status
    &["clock"],                          // time — a dedicated end-cap (switcher trails)
];

struct ModuleEntry {
    /// Stable instance id = `stable_id(name)`; routing + recipe key (RFC 0004).
    id: u64,
    /// The placement **key** — the instance's identity across reloads.
    name: String,
    module: Box<dyn Module>,
    /// The resolved config this instance was last (re)built/reconfigured with, so a
    /// reconcile can tell "unchanged" (keep state) from "changed" (reconfigure).
    cfg: toml::Value,
    disabled: bool,
}

impl ModuleEntry {
    fn new(id: u64, name: String, module: Box<dyn Module>, cfg: toml::Value) -> Self {
        ModuleEntry {
            id,
            name,
            module,
            cfg,
            disabled: false,
        }
    }
}

fn main() -> iced_layershell::Result {
    env_logger::init();

    match std::env::args().nth(1).as_deref() {
        Some("install") => {
            match install::run() {
                Ok(msg) => println!("{msg}"),
                Err(e) => {
                    eprintln!("ezbar: {e}");
                    std::process::exit(1);
                }
            }
            return Ok(());
        }
        Some("package") => {
            // RFC 0014 Phase B producer: embed ezbar:manifest + print the registry entry.
            // `ezbar package <plugin.wasm> [sidecar.toml] [-o out.wasm]`
            let a: Vec<String> = std::env::args().skip(2).collect();
            let positional: Vec<&String> = a.iter().take_while(|s| *s != "-o").collect();
            let out = a.iter().position(|s| s == "-o").and_then(|i| a.get(i + 1));
            match positional.first() {
                Some(wasm) => {
                    let res = package::run(
                        std::path::Path::new(wasm),
                        positional.get(1).map(|s| std::path::Path::new(s.as_str())),
                        out.map(|s| std::path::Path::new(s.as_str())),
                    );
                    match res {
                        Ok(entry) => println!("{entry}"),
                        Err(e) => {
                            eprintln!("ezbar package: {e}");
                            std::process::exit(1);
                        }
                    }
                }
                None => {
                    eprintln!("ezbar package: usage: ezbar package <plugin.wasm> [ezbar-plugin.toml] [-o out.wasm]");
                    std::process::exit(2);
                }
            }
            return Ok(());
        }
        Some("remove" | "rm") => {
            match std::env::args().nth(2) {
                Some(id) => match registry::remove(&id) {
                    Ok(msg) => println!("{msg}"),
                    Err(e) => {
                        eprintln!("ezbar remove: {e}");
                        std::process::exit(1);
                    }
                },
                None => {
                    eprintln!("ezbar remove: usage: ezbar remove <id>");
                    std::process::exit(2);
                }
            }
            return Ok(());
        }
        Some("search") => {
            let args: Vec<String> = std::env::args().skip(2).collect();
            let registry = args
                .iter()
                .position(|a| a == "--registry" || a == "-r")
                .and_then(|i| args.get(i + 1).cloned())
                .or_else(|| std::env::var("EZBAR_REGISTRY").ok());
            let term = args
                .iter()
                .take_while(|a| *a != "--registry" && *a != "-r")
                .find(|a| !a.starts_with('-'))
                .cloned()
                .unwrap_or_default();
            match registry {
                Some(reg) => match registry::search(&term, &reg) {
                    Ok(s) => print!("{s}"),
                    Err(e) => {
                        eprintln!("ezbar search: {e}");
                        std::process::exit(1);
                    }
                },
                None => {
                    eprintln!("ezbar search: no registry — pass --registry <dir|git-url> or set EZBAR_REGISTRY");
                    std::process::exit(2);
                }
            }
            return Ok(());
        }
        Some("list") => {
            match registry::list() {
                Ok(s) => print!("{s}"),
                Err(e) => {
                    eprintln!("ezbar list: {e}");
                    std::process::exit(1);
                }
            }
            return Ok(());
        }
        Some("add") => {
            // RFC 0014 Phase C (local): install a plugin from a registry directory.
            // `ezbar add <id> [--registry <dir>]` (else $EZBAR_REGISTRY).
            let args: Vec<String> = std::env::args().skip(2).collect();
            let registry = args
                .iter()
                .position(|a| a == "--registry" || a == "-r")
                .and_then(|i| args.get(i + 1).cloned())
                .or_else(|| std::env::var("EZBAR_REGISTRY").ok());
            let id = args.iter().find(|a| !a.starts_with('-')).cloned();
            match (id, registry) {
                (Some(id), Some(reg)) => match registry::add(&id, &reg) {
                    Ok(msg) => print!("{msg}"),
                    Err(e) => {
                        eprintln!("ezbar add: {e}");
                        std::process::exit(1);
                    }
                },
                (None, _) => {
                    eprintln!("ezbar add: usage: ezbar add <id> [--registry <dir>]");
                    std::process::exit(2);
                }
                (Some(_), None) => {
                    eprintln!(
                        "ezbar add: no registry — pass --registry <dir> or set EZBAR_REGISTRY \
                         (the hosted registry isn't available yet; a local directory works: \
                         <dir>/plugins/<id>/<version>.toml + <version>.wasm)"
                    );
                    std::process::exit(2);
                }
            }
            return Ok(());
        }
        Some("inspect") => {
            // RFC 0014: show what a plugin declares + the grant block to paste (no install,
            // no run). `ezbar inspect <plugin.wasm>`.
            match std::env::args().nth(2) {
                Some(path) => {
                    let p = std::path::Path::new(&path);
                    let id = p.file_stem().and_then(|s| s.to_str()).unwrap_or("plugin");
                    match ezbar::grants::inspect(p, id) {
                        Ok(report) => print!("{report}"),
                        Err(e) => {
                            eprintln!("ezbar inspect: {e}");
                            std::process::exit(1);
                        }
                    }
                }
                None => {
                    eprintln!("ezbar inspect: usage: ezbar inspect <plugin.wasm>");
                    std::process::exit(2);
                }
            }
            return Ok(());
        }
        Some("grant") => {
            // Record explicit consent for a plugin's current bytes (RFC 0014 Phase A) —
            // the re-approval path after a legitimate rebuild/update changed its hash.
            match std::env::args().nth(2) {
                Some(id) => match ezbar::grants::grant_cli(&id) {
                    Ok(msg) => println!("{msg}"),
                    Err(e) => {
                        eprintln!("ezbar: {e}");
                        std::process::exit(1);
                    }
                },
                None => {
                    eprintln!("ezbar grant: usage: ezbar grant <plugin-id>");
                    std::process::exit(2);
                }
            }
            return Ok(());
        }
        Some("msg") => {
            let cmd = std::env::args().skip(2).collect::<Vec<_>>().join(" ");
            if cmd.is_empty() {
                eprintln!("ezbar msg: usage: ezbar msg <reload|preset <name|next|prev>|popup <kind>|volume <up|down|mute>>");
                std::process::exit(2);
            }
            if let Err(e) = ipc::send(&cmd) {
                eprintln!("ezbar msg: {e} (is the bar running?)");
                std::process::exit(1);
            }
            return Ok(());
        }
        Some("--version" | "-V" | "version") => {
            println!("ezbar {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some("--help" | "-h" | "help") => {
            print_help();
            return Ok(());
        }
        Some(other) => {
            eprintln!("ezbar: unknown command '{other}'\n");
            print_help();
            std::process::exit(2);
        }
        None => {}
    }

    // Launcher: re-spawn the bar child forever (restart on crash / monitor change),
    // unless we ARE the child. A short backoff avoids hot-spinning (improves on the Go original).
    if std::env::var("EZBAR_CHILD").as_deref() != Ok("1") {
        // Singleton guard: at most one ezbar supervisor per wayland session.
        // `exec_always ezbar` re-runs on every `swaymsg reload` — which a monitor
        // change commonly triggers — so without this a second supervisor+child
        // stacks a *duplicate bar*. The already-running instance reconciles
        // outputs on hotplug (RFC 0004) and watches its own config, so the new
        // invocation just exits. The lock self-clears when its owner dies, so a
        // crashed instance is cleanly replaced (no stale lockfiles).
        let _singleton = match acquire_singleton() {
            Ok(guard) => guard,
            Err(()) => {
                log::info!("ezbar already running for this session — exiting");
                return Ok(());
            }
        };
        loop {
            let exe = std::env::current_exe().expect("current_exe");
            match std::process::Command::new(exe)
                .env("EZBAR_CHILD", "1")
                .status()
            {
                Ok(s) if s.success() => log::info!("child exited cleanly"),
                Ok(s) => log::error!("child crashed: {:?}", s.code()),
                Err(e) => log::error!("failed to spawn child: {e}"),
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    run_bar()
}

fn print_help() {
    println!(
        "ezbar — a status bar for sway\n\n\
         USAGE:\n    \
         ezbar              run the bar (default)\n    \
         ezbar install      add ezbar to your sway config (idempotent, never edits existing lines)\n    \
         ezbar search <q>   search a registry for plugins (--registry <dir|git-url>)\n    \
         ezbar list         list installed plugins + their consent state + declared caps\n    \
         ezbar add <id>     install a plugin from a registry dir (--registry <dir> or $EZBAR_REGISTRY)\n    \
         ezbar remove <id>  delete an installed plugin (and its consent record; not your config)\n    \
         ezbar inspect <f>  show what a plugin .wasm declares + the [modules.<id>] block to paste\n    \
         ezbar grant <id>   approve a plugin's current bytes for its configured capabilities\n    \
         ezbar package …    embed ezbar:manifest into a built plugin + print its registry entry\n    \
         ezbar --version    print the version\n    \
         ezbar --help       print this help\n\n\
         EZBAR_CHILD=1 ezbar   run a single foreground instance (no respawn)"
    );
}

/// Acquire a per-session singleton lock via a Linux **abstract** socket: the
/// kernel drops it automatically when the owning process exits, so it's
/// self-cleaning (unlike a pidfile, which can go stale after a crash). `Err(())`
/// means another ezbar already holds it for this `(XDG_RUNTIME_DIR,
/// WAYLAND_DISPLAY)` — i.e. this wayland session.
#[cfg(target_os = "linux")]
fn acquire_singleton() -> Result<std::os::unix::net::UnixListener, ()> {
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::net::{SocketAddr, UnixListener};
    let key = format!(
        "ezbar.{}.{}",
        std::env::var("XDG_RUNTIME_DIR").unwrap_or_default(),
        std::env::var("WAYLAND_DISPLAY").unwrap_or_default()
    );
    SocketAddr::from_abstract_name(key.as_bytes())
        .and_then(|addr| UnixListener::bind_addr(&addr))
        .map_err(|_| ())
}

#[cfg(not(target_os = "linux"))]
fn acquire_singleton() -> Result<(), ()> {
    Ok(()) // singleton guard is Linux-only; elsewhere just proceed
}

/// Install the metric sampler the WASM reactor fans out to subscribers (RFC 0012). Maps each
/// `FeedKind` to a current value using the bar's own `/proc`/sysfs readers. `Ping` is deferred
/// (the parameterless feed has no target). The closure runs on the reactor's blocking pool
/// (the cpu read blocks ~100 ms), so reading `/proc` here is fine.
fn install_feed_sampler() {
    use ezbar::sources::{battery, system};
    use ezbar_wasm::FeedKind;
    // net is a *rate*, derived from counter deltas — keep the previous total across calls.
    let net_prev: Arc<Mutex<Option<(std::time::Instant, u64)>>> = Arc::new(Mutex::new(None));
    ezbar_wasm::set_feed_sampler(Arc::new(move |kind| match kind {
        FeedKind::Cpu => Some(system::extract_cpu_usage_value(&system::get_cpu_usage())),
        FeedKind::Memory => Some(system::extract_memory_usage_value(&system::get_memory_usage())),
        FeedKind::Temperature => {
            Some(system::extract_temperature_value(&system::get_cpu_temperature()))
        }
        FeedKind::Battery => battery_percent(&battery::get_battery_status()),
        FeedKind::Net => net_rate(&net_prev),
        FeedKind::Ping => None, // deferred — no target in the v0.1 ABI (RFC 0012 §6)
    }));
}

/// Leading "NN%" of `get_battery_status()` ("NN% [time]" or "--") as a percentage.
fn battery_percent(status: &str) -> Option<f64> {
    status.split('%').next()?.trim().parse::<f64>().ok()
}

/// Total non-loopback throughput in bytes/s (down+up), derived from `/proc/net/dev` counter
/// deltas. Returns `None` on the first read (priming `prev`) and whenever the gap is too long
/// or the counter went backwards (interface reset / sampler respawn) — so a subscriber never
/// sees a garbage spike from a stale `dt` (RFC 0012 §3).
fn net_rate(prev: &Mutex<Option<(std::time::Instant, u64)>>) -> Option<f64> {
    let total = net_total_bytes()?;
    let now = std::time::Instant::now();
    let mut g = prev.lock().unwrap_or_else(|e| e.into_inner());
    let rate = match *g {
        Some((t0, b0)) => {
            let dt = now.duration_since(t0).as_secs_f64();
            if dt <= 0.0 || dt > 5.0 || total < b0 {
                None // stale/respawn/reset → re-prime, emit nothing this tick
            } else {
                Some((total - b0) as f64 / dt)
            }
        }
        None => None, // first read just primes prev
    };
    *g = Some((now, total));
    rate
}

/// Sum rx+tx byte counters across non-loopback interfaces (`/proc/net/dev`). By design this
/// is *all* non-`lo` traffic — physical, VPN (`tailscale0`), and bridges (`docker0`) included
/// — so the `net` feed is a whole-machine throughput gauge, not a per-interface one.
fn net_total_bytes() -> Option<u64> {
    let data = std::fs::read_to_string("/proc/net/dev").ok()?;
    let mut total = 0u64;
    for line in data.lines().skip(2) {
        if let Some((name, rest)) = line.split_once(':') {
            if name.trim() == "lo" {
                continue;
            }
            let cols: Vec<&str> = rest.split_whitespace().collect();
            if cols.len() >= 9 {
                total += cols[0].parse::<u64>().unwrap_or(0); // rx bytes
                total += cols[8].parse::<u64>().unwrap_or(0); // tx bytes
            }
        }
    }
    Some(total)
}

fn run_bar() -> iced_layershell::Result {
    // Memory: glibc hoards freed heap in its arenas instead of returning it to the
    // OS, so the startup churn (wgpu + wasmtime warmup) strands a few hundred MB of
    // free-but-resident pages — measured ~330 MB that `malloc_trim(0)` hands straight
    // back. Reclaim on a slow timer from a dedicated thread so it never touches the
    // render path; glibc-only (a no-op elsewhere, so just compile it out).
    #[cfg(target_env = "gnu")]
    std::thread::spawn(|| loop {
        std::thread::sleep(Duration::from_secs(60));
        // SAFETY: `malloc_trim` is thread-safe and only releases free pages back to
        // the kernel — it never frees live allocations.
        unsafe {
            libc::malloc_trim(0);
        }
    });

    // Default to a Nerd Font so the icon glyphs render; overridable via [bar].font.
    let cfg = config::load();
    // Discover WASM plugins (RFC 0006) before any module is built so their ids
    // are placeable like built-ins.
    if let Some(dir) = config::plugins_dir() {
        modules::register_wasm_plugins(&dir);
    }
    // Wire the bar's metric readers into the WASM reactor so a sandboxed plugin can draw a
    // cpu/mem/temp/battery/net graph via `feed-subscribe` (RFC 0012). The reactor lives below
    // us in the dep graph and can't read `/proc`; it calls this closure on its blocking pool.
    install_feed_sampler();
    // Wire the bar's sway snapshot into the reactor for the read-only `sway-snapshot` capability
    // (RFC 0013): the reactor can't open its own sway connection, so it reads the bar's.
    ezbar_wasm::set_sway_source(Arc::new(|| {
        let s = ezbar::sources::sway::snapshot();
        ezbar_wasm::SwaySnapshot {
            workspaces: s
                .workspaces
                .iter()
                .map(|w| ezbar_wasm::SwayWorkspaceInfo {
                    name: w.name.clone(),
                    focused: w.focused,
                    visible: w.visible,
                    urgent: w.urgent,
                })
                .collect(),
            title: s.title.clone(),
        }
    }));
    let name = cfg
        .bar
        .font
        .clone()
        .unwrap_or_else(|| "JetBrainsMono Nerd Font".to_string());
    let mut font = iced::Font::with_name(Box::leak(name.into_boxed_str()));
    font.weight = cfg.bar.weight.iced();
    daemon(Bar::new, Bar::namespace, Bar::update, Bar::view)
        .settings(Settings {
            layer_settings: LayerShellSettings {
                start_mode: StartMode::Background,
                ..Default::default()
            },
            ..Default::default()
        })
        .style(Bar::style)
        .subscription(Bar::subscription)
        // Cap the async runtime at 2 workers (see `BoundedExecutor`). iced's default
        // builds one worker per core; a bar's async workload is a few timers + HTTP
        // polls, so 16 workers just spread the allocation churn across 16 glibc
        // arenas. (tokio 1.52 ignores `TOKIO_WORKER_THREADS`, so it must be explicit.)
        .executor::<BoundedExecutor>()
        .default_text_size(14.0)
        .default_font(font)
        .run()
}

/// A tokio runtime capped to 2 worker threads, mirroring iced's own
/// `tokio::runtime::Runtime` executor but with an explicit worker count. A status
/// bar needs almost no concurrency, and fewer worker threads means glibc spins up
/// far fewer per-thread arenas (each a 64 MB heap it never hands back to the OS).
struct BoundedExecutor(tokio::runtime::Runtime);

/// The bar's async runtime handle, captured when iced builds `BoundedExecutor` — which
/// happens before the app boots — so the WASM reactor (RFC 0008) spawns plugin tasks on
/// this exact runtime. Avoids both a second runtime and any `Handle::current()` ordering
/// risk in `Bar::new`.
static RT_HANDLE: std::sync::OnceLock<tokio::runtime::Handle> = std::sync::OnceLock::new();

impl iced::Executor for BoundedExecutor {
    fn new() -> Result<Self, std::io::Error> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let _ = RT_HANDLE.set(rt.handle().clone());
        Ok(BoundedExecutor(rt))
    }

    #[allow(clippy::let_underscore_future)]
    fn spawn(&self, future: impl std::future::Future<Output = ()> + Send + 'static) {
        let _ = self.0.spawn(future);
    }

    fn block_on<T>(&self, future: impl std::future::Future<Output = T>) -> T {
        self.0.block_on(future)
    }

    fn enter<R>(&self, f: impl FnOnce() -> R) -> R {
        let _guard = self.0.enter();
        f()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PopupKind {
    /// the ▾ preset switcher — the only host-owned popup; module popups (calendar,
    /// stock, kubectl) go through the module-popup path (`HostRequest::OpenPopup`).
    Switcher,
}

/// One bar layer surface, bound to a named sway output (RFC 0004 per-output set).
struct BarSurface {
    id: window::Id,
    output: String,
    /// the output's logical (layout) width — same space as iced's logical cursor x,
    /// so the popup clamp stays scale-correct on fractional-scale outputs.
    width: u32,
}

struct Bar {
    /// One layer surface per matching output (RFC 0004), reconciled on hotplug.
    bars: Vec<BarSurface>,
    popup: Option<(window::Id, PopupKind)>,
    module_popup: Option<(window::Id, u64, PopupMode)>,
    modules: Vec<ModuleEntry>,

    // popup anchoring: last cursor x over a bar, so a popup opens above the
    // widget the user interacted with (RFC 0001 slot-derived).
    cursor_x: f32,
    // the output whose bar the cursor was last over, so a popup opens there.
    cursor_output: Option<String>,

    // config + resolved module theme (RFC 0002)
    config: Config,
    theme: ThemeTokens,

    // width of the output the cursor is over, so popups can be clamped on-screen
    screen_w: u32,

    // RFC 0004: the live bar surface's current geometry position, so a reconcile
    // can diff against it and re-anchor in place instead of re-rolling.
    bar_pos: config::Position,

    // RFC 0002/0004: per-instance subscription generation. Bumped when a module is
    // reconfigured/reconstructed so the host's `.with((id, gen))` recipe key changes
    // and iced re-rolls that instance's streams (the old config's streams drop).
    generation: HashMap<u64, u64>,

    // RFC 0008: the bar's async runtime handle, captured in `new` (where iced runs us
    // inside the executor context). WASM plugins' reactor tasks are spawned on it, so
    // there's no second runtime; threaded into `build`/`reconcile_modules`.
    rt: tokio::runtime::Handle,
}

/// A connected sway output a bar may be placed on.
struct OutputInfo {
    name: String,
    width: u32,
}

/// All active sway outputs (name + logical width), via sway IPC. `rect` is in
/// sway's logical layout coordinates — the same space as iced's logical cursor —
/// so widths are scale-correct for popup clamping. Empty on failure (e.g. not
/// under sway) — the bar then simply has no surfaces until one appears.
fn sway_outputs() -> Vec<OutputInfo> {
    swayipc::Connection::new()
        .ok()
        .and_then(|mut c| c.get_outputs().ok())
        .map(|outs| {
            outs.into_iter()
                .filter(|o| o.active)
                .map(|o| OutputInfo {
                    name: o.name,
                    width: o.rect.width.max(0) as u32,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The outputs a bar should occupy: active outputs matching `[bar].outputs`
/// (RFC 0004). `"all"` ⇒ every output; `["DP-1", …]` ⇒ just the named ones.
fn desired_outputs(cfg: &Config) -> Vec<OutputInfo> {
    sway_outputs()
        .into_iter()
        .filter(|o| cfg.bar.outputs.matches(&o.name))
        .collect()
}

/// The pure output-churn decision (RFC 0004): given the `desired` output names and the
/// currently-`tracked` `(output, surface-id)` pairs, return `(close, create)` — the surface
/// ids to close (their output is gone or de-selected) and the output names that need a new
/// surface (desired but untracked). **Dedups**: an output already tracked, or listed twice in
/// `desired`, yields at most ONE surface — the guard against the "two-bars saga" (a duplicate
/// bar stacked on one output). Generic over the id type so it unit-tests without `window::Id`.
fn plan_surfaces<Id: Copy>(desired: &[String], tracked: &[(String, Id)]) -> (Vec<Id>, Vec<String>) {
    use std::collections::HashSet;
    let desired_set: HashSet<&str> = desired.iter().map(String::as_str).collect();
    let tracked_set: HashSet<&str> = tracked.iter().map(|(n, _)| n.as_str()).collect();
    let close = tracked
        .iter()
        .filter(|(n, _)| !desired_set.contains(n.as_str()))
        .map(|(_, id)| *id)
        .collect();
    let mut seen = HashSet::new();
    let create = desired
        .iter()
        .filter(|n| !tracked_set.contains(n.as_str()) && seen.insert(n.as_str()))
        .cloned()
        .collect();
    (close, create)
}

#[to_layer_message(multi)]
#[derive(Debug, Clone)]
enum Message {
    VolumeAdjust(i32), // IPC volume keybind path (0 = mute, ±1 = change)
    SelectPreset(String),
    Ipc(String),
    OpenPopup(PopupKind),
    ClosePopup,
    ConfigReloaded(Result<Config, String>),
    WindowClosed(window::Id),
    Cursor(window::Id, f32),
    /// A sway output appeared/disappeared/changed — reconcile the surface set.
    OutputsChanged,
    Noop,
    ModuleMsg {
        instance: u64,
        msg: ModMsg,
    },
}

fn iced_layer(l: config::Layer) -> Layer {
    match l {
        config::Layer::Background => Layer::Background,
        config::Layer::Bottom => Layer::Bottom,
        config::Layer::Top => Layer::Top,
        config::Layer::Overlay => Layer::Overlay,
    }
}

/// The full layer-shell geometry for the bar at `pos`, derived purely from config.
/// Single source of truth: both surface creation (`bar_settings`) and the live
/// reconcile (`reconcile_bar_geometry`) read it, so they can never drift (RFC 0004).
struct BarGeom {
    anchor: Anchor,
    /// (top, right, bottom, left) — layer-shell margin order.
    margin: (i32, i32, i32, i32),
    exclusive_zone: i32,
    /// (width, height); width 0 = span the anchored axis.
    size: (u32, u32),
    layer: Layer,
}

fn bar_geom(b: &config::Bar, pos: config::Position) -> BarGeom {
    let h = b.height.max(1);
    let m = b.margin;
    // Top or bottom edge; span the full width minus L/R margins.
    let edge = match pos {
        config::Position::Top => Anchor::Top,
        config::Position::Bottom => Anchor::Bottom,
    };
    // Reserve the bar's height plus its near-edge gap so windows never overlap it.
    let near_gap = match pos {
        config::Position::Top => m.top,
        config::Position::Bottom => m.bottom,
    };
    BarGeom {
        anchor: edge | Anchor::Left | Anchor::Right,
        margin: (m.top, m.right, m.bottom, m.left),
        exclusive_zone: h as i32 + near_gap.max(0),
        size: (0, h),
        layer: iced_layer(b.layer),
    }
}

fn bar_settings(cfg: &Config, pos: config::Position, output: &str) -> NewLayerShellSettings {
    let g = bar_geom(&cfg.bar, pos);
    NewLayerShellSettings {
        size: Some(g.size),
        exclusive_zone: Some(g.exclusive_zone),
        anchor: g.anchor,
        margin: Some(g.margin),
        layer: g.layer,
        keyboard_interactivity: KeyboardInteractivity::None,
        output_option: OutputOption::OutputName(output.to_string()),
        namespace: Some("ezbar".to_string()),
        ..Default::default()
    }
}

fn popup_size(kind: PopupKind) -> (u32, u32) {
    match kind {
        PopupKind::Switcher => (220, 280),
    }
}

/// One placeable item: routing `key`, module `type_id`, and inline `config`.
struct Placed {
    key: String,
    type_id: String,
    config: toml::Value,
}

fn empty_cfg() -> toml::Value {
    toml::Value::Table(Default::default())
}

/// Resolve a zone's entries (or the shipped default) into ordered `Placed` items.
fn resolve_zone(entries: &[config::Entry], default: &[&str], out: &mut Vec<Placed>) {
    if entries.is_empty() {
        for id in default {
            out.push(Placed {
                key: id.to_string(),
                type_id: id.to_string(),
                config: empty_cfg(),
            });
        }
    } else {
        for e in entries {
            resolve_entry(e, out);
        }
    }
}

fn resolve_entry(e: &config::Entry, out: &mut Vec<Placed>) {
    match e {
        config::Entry::Id(id) => out.push(Placed {
            key: id.clone(),
            type_id: id.clone(),
            config: empty_cfg(),
        }),
        config::Entry::Spec(s) => out.push(Placed {
            key: s.key.clone().unwrap_or_else(|| s.id.clone()),
            type_id: s.id.clone(),
            config: s.config.clone(),
        }),
        config::Entry::Group(g) => {
            for m in g {
                resolve_entry(m, out);
            }
        }
    }
}

/// Resolve the right zone into **groups** (RFC 0005) — each becomes a sub-island in
/// `islands` style. A top-level `Entry::Group` is one group; a bare entry is its own
/// singleton group; an empty zone uses the shipped default groups.
fn resolve_right_groups(config: &Config) -> Vec<Vec<Placed>> {
    let mut groups: Vec<Vec<Placed>> = if config.right.is_empty() {
        DEFAULT_RIGHT_GROUPS
            .iter()
            .map(|g| {
                g.iter()
                    .map(|id| Placed {
                        key: id.to_string(),
                        type_id: id.to_string(),
                        config: empty_cfg(),
                    })
                    .collect()
            })
            .collect()
    } else {
        config
            .right
            .iter()
            .map(|e| {
                let mut g = Vec::new();
                resolve_entry(e, &mut g); // a Group flattens to its members; a bare entry → 1
                g
            })
            .collect()
    };
    // Each discovered WASM plugin (RFC 0006) gets its own trailing pill, unless the
    // user already placed it in the right zone — so a dropped-in `.wasm` is a
    // distinct, obvious chip rather than glued onto a neighbour.
    let plugins = {
        let placed: std::collections::HashSet<&str> = groups
            .iter()
            .flatten()
            .map(|p| p.type_id.as_str())
            .collect();
        unplaced_wasm_plugins(&placed)
    };
    // Insert plugin pills just BEFORE the final group (the clock end-cap by
    // default), so the trailing ▾ switcher stays on the end-cap and a plugin's
    // pill is its own distinct island — not glued next to the switcher.
    let end_cap = groups.pop();
    groups.extend(plugins.into_iter().map(|p| vec![p]));
    groups.extend(end_cap);
    groups
}

/// WASM plugins the user did not explicitly place (RFC 0006) — each becomes its
/// own right-cluster pill so dropping a `.wasm` in the plugins dir just works.
fn unplaced_wasm_plugins(placed: &std::collections::HashSet<&str>) -> Vec<Placed> {
    modules::wasm_plugin_ids()
        .into_iter()
        .filter(|id| !placed.contains(id.as_str()))
        .map(|id| Placed {
            key: id.clone(),
            type_id: id,
            config: empty_cfg(),
        })
        .collect()
}

/// All placed items across the three zones, in order (zones fall back to defaults).
fn all_placed(config: &Config) -> Vec<Placed> {
    let mut items = Vec::new();
    resolve_zone(&config.left, &["workspaces"], &mut items);
    resolve_zone(&config.center, &["window_title"], &mut items);
    // resolve_right_groups already appends a group per discovered WASM plugin.
    items.extend(resolve_right_groups(config).into_iter().flatten());
    items
}

/// Stable instance id from the placement `key` (RFC 0004 identity). The same key
/// maps to the same id across reloads — independent of zone and order — so a
/// reconcile can keep an unchanged instance's state and recipes. `DefaultHasher`
/// is fixed-keyed, hence deterministic within and across runs.
fn stable_id(key: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut h);
    h.finish()
}

/// A desired module instance: routing `key`, module `type_id`, resolved config.
struct ModuleSpec {
    key: String,
    type_id: String,
    cfg: toml::Value,
}

/// The desired module instances from the resolved placement — one per unique `key`,
/// in placement order, each configured from `[modules.<id>]` overlaid by the entry's
/// inline `config`. The single source of truth for both initial build and reconcile.
fn desired_module_specs(config: &Config) -> Vec<ModuleSpec> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for p in all_placed(config) {
        // Skip host chrome such as `switcher`; placeable widgets are modules.
        if !modules::is_module(&p.type_id) {
            continue;
        }
        // a key identifies exactly one module instance
        if !seen.insert(p.key.clone()) {
            log::warn!("duplicate module key {:?}; ignoring the second", p.key);
            continue;
        }
        let cfg = config::merge_module_config(config.modules.get(&p.type_id), &p.config);
        out.push(ModuleSpec {
            key: p.key,
            type_id: p.type_id,
            cfg,
        });
    }
    out
}

/// Build the live module set (RFC 0001 factory) from the desired specs, keyed by
/// `stable_id(key)`.
fn build_modules(config: &Config, rt: &tokio::runtime::Handle) -> Vec<ModuleEntry> {
    desired_module_specs(config)
        .into_iter()
        .filter_map(|s| {
            let id = stable_id(&s.key);
            modules::build(&s.type_id, id, &s.cfg, rt)
                .map(|m| ModuleEntry::new(id, s.key, m, s.cfg))
        })
        .collect()
}

fn parse_popup_kind(s: &str) -> Option<PopupKind> {
    match s {
        "switcher" | "theme" => Some(PopupKind::Switcher),
        _ => None,
    }
}

/// Daemon side: listen on the `ezbar` unix socket and emit each line as `Message::Ipc`.
fn ipc_stream() -> impl Stream<Item = Message> {
    iced::stream::channel(
        50,
        |mut out: iced::futures::channel::mpsc::Sender<Message>| async move {
            let path = ipc::socket_path();
            // Probe before unlinking: if a live instance is already listening, DON'T take
            // the socket over — the old unconditional remove+bind silently hijacked IPC from
            // a running bar (a second `ezbar` launch stole `ezbar msg` routing). Only clear a
            // socket nothing answers on (a stale leftover from a crash); otherwise run without
            // IPC and leave the live instance owning it.
            if path.exists() {
                match std::os::unix::net::UnixStream::connect(&path) {
                    Ok(_) => {
                        log::warn!(
                            "ipc: {} already has a live listener — another ezbar is running; \
                             this instance runs without IPC rather than hijack it",
                            path.display()
                        );
                        return;
                    }
                    Err(_) => {
                        let _ = std::fs::remove_file(&path); // stale socket — safe to clear
                    }
                }
            }
            let listener = match tokio::net::UnixListener::bind(&path) {
                Ok(l) => l,
                Err(e) => {
                    log::error!("ipc: bind {}: {e}", path.display());
                    return;
                }
            };
            loop {
                match listener.accept().await {
                    Ok((stream, _)) => {
                        use tokio::io::AsyncBufReadExt;
                        let mut reader = tokio::io::BufReader::new(stream);
                        let mut line = String::new();
                        while reader.read_line(&mut line).await.unwrap_or(0) > 0 {
                            let cmd = line.trim().to_string();
                            if !cmd.is_empty() {
                                use iced::futures::SinkExt;
                                let _ = out.send(Message::Ipc(cmd)).await;
                            }
                            line.clear();
                        }
                    }
                    Err(e) => log::warn!("ipc: accept: {e}"),
                }
            }
        },
    )
}

const MODULE_POPUP_SIZE: (u32, u32) = (480, 400);

impl Bar {
    fn new() -> (Self, Task<Message>) {
        let config = config::load();
        let bar_pos = config.bar.position;
        let theme = config.theme_tokens();
        // One surface per matching output (RFC 0004). Empty is valid — the
        // output-event subscription will add surfaces as outputs appear.
        let outputs = desired_outputs(&config);
        let screen_w = outputs.first().map(|o| o.width).unwrap_or(1920);
        let mut bars = Vec::new();
        let mut opens = Vec::new();
        for o in outputs {
            let id = window::Id::unique();
            opens.push(Task::done(Message::NewLayerShell {
                settings: bar_settings(&config, bar_pos, &o.name),
                id,
            }));
            bars.push(BarSurface {
                id,
                output: o.name,
                width: o.width,
            });
        }
        // The bar's runtime — the one we drive WASM plugins on (RFC 0008 §3.1).
        // Captured in `BoundedExecutor::new` (runs before boot); `Handle::current()`
        // is a defensive fallback (iced also runs `new` inside the executor context).
        let rt = RT_HANDLE
            .get()
            .cloned()
            .unwrap_or_else(tokio::runtime::Handle::current);
        let bar = Bar {
            bars,
            popup: None,
            module_popup: None,
            modules: build_modules(&config, &rt),
            cursor_x: 0.0,
            cursor_output: None,
            config,
            theme,
            screen_w,
            bar_pos,
            generation: HashMap::new(),
            rt,
        };
        let open = Task::batch(opens);
        // Dev/screenshot hook: open the switcher popup on startup for capture.
        if std::env::var("EZBAR_OPEN_POPUP").is_ok() {
            return (
                bar,
                Task::batch([open, Task::done(Message::OpenPopup(PopupKind::Switcher))]),
            );
        }
        (bar, open)
    }

    /// Is `id` one of our bar surfaces?
    fn is_bar(&self, id: window::Id) -> bool {
        self.bars.iter().any(|b| b.id == id)
    }

    fn namespace() -> String {
        "ezbar".to_string()
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::VolumeAdjust(dir) => {
                // Route into the volume module (it changes the level AND refreshes its on-bar
                // value in one update — no lag waiting for its next 1s poll). Fall back to
                // poking the source directly only if no volume module is placed (a headless
                // keybind on a bar without the pill).
                if let Some(instance) = self
                    .modules
                    .iter()
                    .find(|e| !e.disabled && e.module.id() == "volume")
                    .map(|e| e.id)
                {
                    Task::done(Message::ModuleMsg {
                        instance,
                        msg: modules::volume::adjust_msg(dir),
                    })
                } else {
                    Task::perform(
                        async move {
                            let _ = tokio::task::spawn_blocking(move || {
                                if dir == 0 {
                                    volume::toggle_mute();
                                } else {
                                    volume::change_volume(dir);
                                }
                            })
                            .await;
                        },
                        |()| Message::Noop,
                    )
                }
            }
            Message::SelectPreset(name) => {
                // Persist the choice (state file, never config.toml), then reload so
                // the preset applies live through the theme path. Closes the popup.
                if let Err(e) = config::save_active_preset(&name) {
                    log::warn!("could not save preset selection: {e}");
                }
                let close = self.close_popup_task();
                let applied = self.apply_config(config::load());
                Task::batch([close, applied])
            }
            Message::Ipc(cmd) => self.handle_ipc(&cmd),
            Message::OpenPopup(kind) => {
                // One popup at a time: a hardcoded popup also closes any module popup.
                let close_mod = self.close_module_popup_any();
                // Toggle off if the same popup is already open.
                if let Some((pid, k)) = self.popup {
                    if k == kind {
                        self.popup = None;
                        return Task::batch([close_mod, iced::window::close(pid)]);
                    }
                    let close = iced::window::close(pid);
                    let (id, open) = self.open_popup(kind);
                    self.popup = Some((id, kind));
                    return Task::batch([close_mod, close, open]);
                }
                let (id, open) = self.open_popup(kind);
                self.popup = Some((id, kind));
                Task::batch([close_mod, open])
            }
            Message::ClosePopup => self.close_popup_task(),
            Message::ConfigReloaded(Ok(cfg)) => {
                log::info!("config reloaded");
                self.apply_config(cfg)
            }
            Message::ConfigReloaded(Err(e)) => {
                log::warn!("config reload failed ({e}); keeping previous config");
                Task::none()
            }
            Message::Cursor(id, x) => {
                if let Some(b) = self.bars.iter().find(|b| b.id == id) {
                    self.cursor_x = x;
                    self.screen_w = b.width;
                    self.cursor_output = Some(b.output.clone());
                }
                Task::none()
            }
            Message::OutputsChanged => self.reconcile_surfaces(),
            Message::WindowClosed(id) => {
                if self.is_bar(id) {
                    log::info!(
                        "WindowClosed: bar surface {id:?} closed by compositor — drop + reconcile"
                    );
                    // A bar surface went away (monitor unplugged/slept). Drop it and
                    // reconcile — if the output is truly gone it stays gone; if it
                    // returns the output-event path re-adds it. We do NOT exit the
                    // whole bar over one output anymore (RFC 0004). This is the
                    // compositor-initiated close (a self-initiated reconcile close
                    // removes the surface from `self.bars` first, so it never reaches
                    // here); a transient popup may have lived on this output, so close
                    // it too — `reconcile_surfaces` won't (it closed nothing itself).
                    self.bars.retain(|b| b.id != id);
                    let popups = self.close_any_popup();
                    let surfaces = self.reconcile_surfaces();
                    return Task::batch([popups, surfaces]);
                }
                if let Some((pid, _)) = self.popup {
                    if pid == id {
                        self.popup = None;
                    }
                }
                if let Some((pid, _, _)) = self.module_popup {
                    if pid == id {
                        self.module_popup = None;
                    }
                }
                Task::none()
            }
            Message::ModuleMsg { instance, msg } => {
                let idx = match self
                    .modules
                    .iter()
                    .position(|e| e.id == instance && !e.disabled)
                {
                    Some(i) => i,
                    None => return Task::none(),
                };
                // RFC 0001 phase-1 panic safety: contain a panicking module to its
                // own `update` and tear it down (show an error chip) rather than
                // crashing the bar. `view`/`canvas::draw` panics are NOT contained
                // — the launcher respawn is their recovery.
                let resp = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    self.modules[idx].module.update(msg)
                })) {
                    Ok(r) => r,
                    Err(_) => {
                        log::error!(
                            "module '{}' panicked in update; disabling",
                            self.modules[idx].name
                        );
                        self.modules[idx].disabled = true;
                        return self.close_module_popup_of(instance);
                    }
                };
                let mut tasks: Vec<Task<Message>> = vec![resp
                    .task
                    .map(move |m| Message::ModuleMsg { instance, msg: m })];
                for req in resp.requests {
                    tasks.push(self.handle_host_request(instance, req));
                }
                Task::batch(tasks)
            }
            _ => Task::none(),
        }
    }

    /// Apply a typed host request from a module (RFC 0001: control never rides
    /// the erased `ModMsg`). Enforces one popup at a time.
    fn handle_host_request(&mut self, instance: u64, req: HostRequest) -> Task<Message> {
        match req {
            HostRequest::OpenPopup(mode) => {
                if let Some((pid, inst, _)) = self.module_popup {
                    if inst == instance {
                        // toggle off
                        self.module_popup = None;
                        return iced::window::close(pid);
                    }
                }
                let close_existing = self.close_any_popup();
                let id = window::Id::unique();
                self.module_popup = Some((id, instance, mode));
                // a module may request a content-sized popup (e.g. a small wasm chart)
                let size = self
                    .modules
                    .iter()
                    .find(|e| e.id == instance)
                    .and_then(|e| e.module.popup_size())
                    .unwrap_or(MODULE_POPUP_SIZE);
                let left = self.popup_left_margin(size.0);
                let open = Task::done(Message::NewLayerShell {
                    settings: self.popup_settings(size, left, matches!(mode, PopupMode::Hover)),
                    id,
                });
                Task::batch([close_existing, open])
            }
            HostRequest::ClosePopup => {
                if let Some((pid, inst, _)) = self.module_popup {
                    if inst == instance {
                        self.module_popup = None;
                        return iced::window::close(pid);
                    }
                }
                Task::none()
            }
        }
    }

    fn close_any_popup(&mut self) -> Task<Message> {
        let mut tasks = Vec::new();
        if let Some((pid, _)) = self.popup.take() {
            tasks.push(iced::window::close(pid));
        }
        if let Some((pid, _, _)) = self.module_popup.take() {
            tasks.push(iced::window::close(pid));
        }
        Task::batch(tasks)
    }

    fn close_module_popup_of(&mut self, instance: u64) -> Task<Message> {
        if let Some((pid, inst, _)) = self.module_popup {
            if inst == instance {
                self.module_popup = None;
                return iced::window::close(pid);
            }
        }
        Task::none()
    }

    fn close_module_popup_any(&mut self) -> Task<Message> {
        if let Some((pid, _, _)) = self.module_popup.take() {
            iced::window::close(pid)
        } else {
            Task::none()
        }
    }

    /// Left margin so a `popup_w`-wide popup is centered above the cursor (the widget
    /// that triggered it), clamped so it always stays fully on the output — both
    /// edges, so a right-anchored widget (e.g. the ▾ switcher) opens visibly.
    fn popup_left_margin(&self, popup_w: u32) -> i32 {
        let max_left = (self.screen_w as i32 - popup_w as i32).max(0);
        (self.cursor_x as i32 - popup_w as i32 / 2).clamp(0, max_left)
    }

    /// Layer-shell settings for a popup, anchored to the **same edge as the bar** so
    /// it floats just off the bar (below a top bar, above a bottom bar) and on the
    /// output the cursor is over (RFC 0004 multi-output). `left_margin` places its
    /// left edge under the triggering widget.
    fn popup_settings(
        &self,
        size: (u32, u32),
        left_margin: i32,
        events_transparent: bool,
    ) -> NewLayerShellSettings {
        // Clear the bar: its height + the near-edge gap it may float by + a hair.
        let m = self.config.bar.margin;
        let (edge, offset) = match self.bar_pos {
            config::Position::Top => (Anchor::Top, m.top.max(0)),
            config::Position::Bottom => (Anchor::Bottom, m.bottom.max(0)),
        };
        let clear = self.config.bar.height as i32 + offset + 6;
        // margin order: (top, right, bottom, left).
        let margin = match self.bar_pos {
            config::Position::Top => (clear, 0, 0, left_margin),
            config::Position::Bottom => (0, 0, clear, left_margin),
        };
        let output_option = match &self.cursor_output {
            Some(name) => OutputOption::OutputName(name.clone()),
            None => OutputOption::None,
        };
        NewLayerShellSettings {
            size: Some(size),
            exclusive_zone: None,
            anchor: edge | Anchor::Left,
            layer: Layer::Overlay,
            margin: Some(margin),
            keyboard_interactivity: KeyboardInteractivity::None,
            events_transparent,
            output_option,
            namespace: Some("ezbar-popup".to_string()),
        }
    }

    fn open_popup(&self, kind: PopupKind) -> (window::Id, Task<Message>) {
        let id = window::Id::unique();
        let size = popup_size(kind);
        let left = self.popup_left_margin(size.0);
        (
            id,
            Task::done(Message::NewLayerShell {
                settings: self.popup_settings(size, left, false),
                id,
            }),
        )
    }

    fn wrap_popup<'a>(&self, body: Element<'a, Message>) -> Element<'a, Message> {
        container(body)
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(12)
            .style(self.popup_style())
            .into()
    }

    /// Themed popup container style (dark, square, hairline border) from config.
    fn popup_style(&self) -> impl Fn(&iced::Theme) -> container::Style {
        let t = &self.config.theme;
        let base = t.background.base().0;
        let bg = Color::from_rgba(base[0], base[1], base[2], t.popup.opacity);
        let radius = t.popup.radius;
        let bw = t.border.width;
        let bc = t.border.color.iced();
        let text = t.text.iced();
        move |_theme: &iced::Theme| container::Style {
            background: Some(Background::Color(bg)),
            border: Border {
                color: bc,
                width: bw,
                radius: radius.into(),
            },
            text_color: Some(text),
            ..Default::default()
        }
    }

    fn close_popup_task(&mut self) -> Task<Message> {
        if let Some((pid, _)) = self.popup.take() {
            iced::window::close(pid)
        } else {
            Task::none()
        }
    }

    fn view(&self, id: window::Id) -> Element<'_, Message> {
        if self.is_bar(id) {
            // The same chip row renders on every output's bar surface.
            return self.bar_view();
        }
        if let Some((pid, kind)) = self.popup {
            if id == pid {
                return self.popup_view(kind);
            }
        }
        if let Some((pid, instance, _mode)) = self.module_popup {
            if id == pid {
                if let Some(entry) = self.modules.iter().find(|e| e.id == instance) {
                    let ctx = Ctx {
                        instance_id: instance,
                        theme: &self.theme,
                    };
                    if let Some(content) = entry.module.popup(&ctx) {
                        let mapped = content.map(move |m| Message::ModuleMsg { instance, msg: m });
                        return self.wrap_popup(mapped);
                    }
                }
            }
        }
        Space::new().into()
    }

    /// Render an RFC 0001 module by its `id` (looked up in the live module list).
    fn render_module(&self, id: &str) -> Option<Element<'_, Message>> {
        let entry = self.modules.iter().find(|e| e.name == id)?;
        let instance = entry.id;
        if !entry.disabled && !entry.module.visible() {
            return None; // nothing to show (e.g. battery on a desktop)
        }
        if entry.disabled {
            return Some(
                text(format!(" {}", entry.name))
                    .color(Color::from_rgb(1.0, 0.3, 0.3))
                    .into(),
            );
        }
        let ctx = Ctx {
            instance_id: instance,
            theme: &self.theme,
        };
        Some(
            entry
                .module
                .view(&ctx)
                .map(move |m| Message::ModuleMsg { instance, msg: m }),
        )
    }

    /// The `▾` preset-switcher button.
    fn switcher_button(&self) -> Element<'_, Message> {
        // The ▾ is the bar's one always-on accent in the right cluster — drawn in the
        // signature colour so lilac (not just the green data) reads as "the bar".
        let accent = self.config.theme.primary.iced();
        mouse_area(
            container(
                text("\u{f107}")
                    .size(self.config.theme.font_size)
                    .color(accent),
            )
            .padding([0, 4])
            .center_y(Length::Fill),
        )
        .on_press(Message::OpenPopup(PopupKind::Switcher))
        .into()
    }

    /// Render host chrome by type id. `None` means the placement is either a module,
    /// rendered by key, or an unknown item that should not appear.
    fn render_widget(&self, id: &str) -> Option<Element<'_, Message>> {
        // Only the ▾ switcher is host chrome now; everything placeable is a Module.
        match id {
            "switcher" => Some(self.switcher_button()),
            _ => None,
        }
    }

    /// The optional per-widget separator *mark* (RFC 0005), or `None` for pure spacing
    /// (the islands default — grouping carries the structure). Built fresh each call.
    fn sep_mark(&self) -> Option<Element<'static, Message>> {
        let s = &self.config.theme.separator;
        let color = s.color.iced();
        Some(match s.style {
            config::SepStyle::None => return None,
            config::SepStyle::Dot => text("\u{00b7}").color(color).into(), // ·
            config::SepStyle::Glyph => text(s.glyph.clone().unwrap_or_else(|| "|".to_string()))
                .color(color)
                .into(),
            config::SepStyle::Line => container(Space::new())
                .width(Length::Fixed(s.width.max(1.0)))
                .height(Length::Fixed(16.0))
                .style(move |_| container::Style {
                    background: Some(Background::Color(color)),
                    ..Default::default()
                })
                .into(),
        })
    }

    /// Build a run of widgets (one group / zone): a module renders by `key`, host
    /// chrome by `type_id`, with `[theme].spacing` between them and the configured
    /// separator mark interposed (never before the `▾` switcher).
    fn build_widgets(&self, items: &[Placed]) -> Element<'_, Message> {
        let mut out: Vec<Element<Message>> = Vec::new();
        for p in items {
            let el = if self.modules.iter().any(|e| e.name == p.key) {
                self.render_module(&p.key)
            } else {
                self.render_widget(&p.type_id)
            };
            if let Some(el) = el {
                if !out.is_empty() && p.type_id != "switcher" {
                    if let Some(mark) = self.sep_mark() {
                        out.push(mark);
                    }
                }
                out.push(el);
            }
        }
        row(out)
            .spacing(self.config.theme.spacing)
            .align_y(Vertical::Center)
            .into()
    }

    /// If `group` is a single module that opted into whole-pill hover
    /// ([`Module::hover_messages`]), return its `(instance, enter, leave)` so the
    /// bar can wrap the styled pill in one `mouse_area`. `None` for multi-widget
    /// groups (the target would be ambiguous) and modules that didn't opt in.
    fn pill_hover(&self, group: &[Placed]) -> Option<(u64, ModMsg, ModMsg)> {
        let [only] = group else { return None };
        let entry = self.modules.iter().find(|e| e.name == only.key)?;
        let (enter, leave) = entry.module.hover_messages()?;
        Some((entry.id, enter, leave))
    }

    /// Wrap a group's rendered `widgets` in the whole-pill hover `mouse_area` when the
    /// group is a single module that opted into it (`hover_messages`). This is the hover
    /// surface WASM plugins rely on — their `view` emits no `mouse_area` of its own, so
    /// without this wrap they get NO hover at all. Used by BOTH the islands and solid
    /// right-cluster builders so hover behaves identically in every bar style (the solid
    /// branch silently lacked this, killing hover for WASM plugins outside islands mode).
    fn with_pill_hover<'a>(
        &self,
        group: &[Placed],
        widgets: Element<'a, Message>,
    ) -> Element<'a, Message> {
        match self.pill_hover(group) {
            Some((instance, enter, leave)) => mouse_area(widgets)
                .on_enter(Message::ModuleMsg { instance, msg: enter })
                .on_exit(Message::ModuleMsg { instance, msg: leave })
                .into(),
            None => widgets,
        }
    }

    fn bar_view(&self) -> Element<'_, Message> {
        // Placement drives which widgets render and in what order; an empty zone falls
        // back to the shipped default. The right zone is GROUPED (RFC 0005): each group
        // becomes a sub-island (islands) or a divider-joined run (solid).
        let mut left = Vec::new();
        let mut center = Vec::new();
        resolve_zone(&self.config.left, &["workspaces"], &mut left);
        resolve_zone(&self.config.center, &["window_title"], &mut center);
        let mut right_groups = resolve_right_groups(&self.config);

        // Place the ▾ switcher per [bar].switcher (unless the user listed it): before
        // the left zone, or trailing the last right group.
        let switcher = || Placed {
            key: "switcher".into(),
            type_id: "switcher".into(),
            config: empty_cfg(),
        };
        let is_switcher = |p: &Placed| p.type_id == "switcher";
        match self.config.bar.switcher {
            SwitcherPos::Left if !left.iter().any(is_switcher) => left.insert(0, switcher()),
            SwitcherPos::Right if !right_groups.iter().flatten().any(is_switcher) => {
                match right_groups.last_mut() {
                    Some(g) => g.push(switcher()),
                    None => right_groups.push(vec![switcher()]),
                }
            }
            _ => {}
        }

        let ws_row = self.build_widgets(&left);
        let title_el = self.build_widgets(&center);
        let gap = self.config.theme.group_gap;

        if matches!(self.config.theme.style, Style::Islands) {
            // Floating SQUARE islands; each group is its own sub-island and the GAPS
            // between them are the separators (RFC 0005).
            let t = &self.config.theme;
            let base = t.background.base().0;
            let pillbg = Color::from_rgba(base[0], base[1], base[2], t.opacity);
            let r = t.radius.group();
            let bw = t.border.width.max(1.0);
            let bc = t.border.color.iced();
            // A soft drop shadow lifts each pill off the wallpaper — framing it on a
            // bright sky as well as a dark patch, where a lilac border alone washes
            // out. This is the "floating islands" read (RFC 0005).
            let pill_style = move |_: &iced::Theme| container::Style {
                background: Some(Background::Color(pillbg)),
                border: Border {
                    color: bc,
                    width: bw,
                    radius: r.into(),
                },
                shadow: iced::Shadow {
                    color: Color::from_rgba(0.0, 0.0, 0.0, 0.45),
                    offset: iced::Vector::new(0.0, 2.0),
                    blur_radius: 8.0,
                },
                ..Default::default()
            };
            // Each visible island floats inside a FULL-HEIGHT cell, so its hit/hover
            // area reaches the screen's top & bottom edges (Fitts's law — slamming the
            // cursor to the border still lands on the bar) even though the pill itself
            // is inset. The 4px float used to live on the bar's outer padding; moving
            // it per-cell keeps the look identical while opening the edge.
            let float = Padding::from([4, 0]);
            let ws_pill = container(
                container(ws_row)
                    .padding([2, 10])
                    .center_y(Length::Fill)
                    .style(pill_style),
            )
            .height(Length::Fill)
            .padding(float);
            let title_pill = container(
                container(title_el)
                    .padding([2, 12])
                    .center_y(Length::Fill)
                    .style(pill_style),
            )
            .height(Length::Fill)
            .padding(float);
            // right cluster: one sub-island per group, `group_gap` between.
            let mut right_pills: Vec<Element<Message>> = Vec::new();
            for (i, g) in right_groups.iter().enumerate() {
                if i > 0 {
                    right_pills.push(Space::new().width(Length::Fixed(gap)).into());
                }
                // full-height cell holding the inset, floating pill
                let cell = container(
                    container(self.build_widgets(g))
                        .padding([2, 10])
                        .center_y(Length::Fill)
                        .style(pill_style),
                )
                .height(Length::Fill)
                .padding(float);
                // Whole-pill hover: when the group is a single opted-in module, the
                // whole cell — float and all, up to the screen edge — is its hover
                // surface. The mouse_area sits OUTSIDE the padding (RFC 0001 popups).
                right_pills.push(self.with_pill_hover(g, cell.into()));
            }
            let right_cluster = row(right_pills).align_y(Vertical::Center);
            container(
                row![
                    ws_pill,
                    Space::new().width(Length::Fill),
                    title_pill,
                    Space::new().width(Length::Fill),
                    right_cluster,
                ]
                .align_y(Vertical::Center),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .padding([0, 10])
            .into()
        } else {
            // Solid slab: one run, groups joined by a divider in a `group_gap` (the
            // separator mark, or a hairline when no explicit style), widgets within a
            // group by `spacing`. Side zones size to content; centre takes the slack
            // (forcing equal thirds clipped a wide cluster on a narrow output).
            let sep_color = self.config.theme.separator.color.iced();
            let mut run: Vec<Element<Message>> = Vec::new();
            for (i, g) in right_groups.iter().enumerate() {
                if i > 0 {
                    let mark = self.sep_mark().unwrap_or_else(|| {
                        container(Space::new())
                            .width(Length::Fixed(1.0))
                            .height(Length::Fixed(16.0))
                            .style(move |_| container::Style {
                                background: Some(Background::Color(sep_color)),
                                ..Default::default()
                            })
                            .into()
                    });
                    run.push(
                        row![
                            Space::new().width(Length::Fixed(gap / 2.0)),
                            mark,
                            Space::new().width(Length::Fixed(gap / 2.0)),
                        ]
                        .align_y(Vertical::Center)
                        .into(),
                    );
                }
                // Same whole-pill hover wrapping as the islands branch — without this the
                // solid bar gave WASM plugins (whose `view` self-wires no hover) NO hover
                // surface at all, so their popups never opened (built-ins like `stock` were
                // unaffected because they wrap their own `mouse_area`). Wrap in a FULL-HEIGHT
                // centered cell first so the hover surface covers the bar-colored padding
                // above/below the chip too (Fitts's law), not just the glyph's tight bounds.
                let cell = container(self.build_widgets(g))
                    .height(Length::Fill)
                    .align_y(Vertical::Center);
                run.push(self.with_pill_hover(g, cell.into()));
            }
            let right_inner: Element<Message> = row(run).align_y(Vertical::Center).into();
            let left_c = container(ws_row)
                .align_x(Horizontal::Left)
                .center_y(Length::Fill)
                .padding([0, 8]);
            let center_c = container(title_el)
                .width(Length::Fill)
                .align_x(Horizontal::Center)
                .center_y(Length::Fill);
            let right_c = container(right_inner)
                .align_x(Horizontal::Right)
                .center_y(Length::Fill)
                .padding([0, 8]);
            container(row![left_c, center_c, right_c].align_y(Vertical::Center))
                .width(Length::Fill)
                .height(Length::Fill)
                .padding([0, 8])
                .into()
        }
    }

    // ---- popups ----

    fn popup_view(&self, kind: PopupKind) -> Element<'_, Message> {
        let body: Element<Message> = match kind {
            PopupKind::Switcher => self.switcher_popup(),
        };
        container(body)
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(12)
            .style(self.popup_style())
            .into()
    }

    fn switcher_popup(&self) -> Element<'_, Message> {
        let accent = self.config.theme.primary.iced();
        let fg = self.config.theme.text.iced();
        let dim = self.config.theme.dim.iced();
        let active = config::active_preset();
        let mut col: Vec<Element<Message>> = vec![text("Theme").size(15).color(dim).into()];
        let names = config::preset_names();
        if names.is_empty() {
            col.push(
                text("(drop presets into ~/.config/ezbar/presets/)")
                    .size(11)
                    .color(dim)
                    .into(),
            );
        }
        for name in names {
            let is_current = active.as_deref() == Some(name.as_str());
            let (marker, label_color) = if is_current {
                ("\u{f00c} ", accent) // check
            } else {
                ("  ", fg)
            };
            // accent-tinted highlight on hover; pointer cursor (a real button)
            let hover = Color { a: 0.18, ..accent };
            col.push(
                button(text(format!("{marker}{name}")).color(label_color))
                    .width(Length::Fill)
                    .padding([3, 6])
                    .on_press(Message::SelectPreset(name.clone()))
                    .style(move |_theme: &iced::Theme, status| {
                        let bg =
                            matches!(status, button::Status::Hovered | button::Status::Pressed)
                                .then_some(Background::Color(hover));
                        button::Style {
                            background: bg,
                            text_color: label_color,
                            border: Border {
                                radius: 4.0.into(),
                                ..Default::default()
                            },
                            ..Default::default()
                        }
                    })
                    .into(),
            );
        }
        scrollable(column(col).spacing(2)).into()
    }

    /// Adopt a freshly-loaded config: re-resolve theme, and rebuild the live module
    /// set **only if** placement or any `[modules.*]` changed (so a pure theme/preset
    /// switch is a cheap re-render that keeps module state). The workspaces widget
    /// reads its style straight from `self.config.theme.workspaces` on render.
    fn apply_config(&mut self, cfg: Config) -> Task<Message> {
        // RFC 0004: surface geometry is reconciled in place, not baked at creation.
        // `position` may also have been overridden live via IPC (`bar_pos`), so diff
        // against the live value, not the previous config's.
        let geom_changed = cfg.bar.position != self.bar_pos
            || cfg.bar.height != self.config.bar.height
            || cfg.bar.margin != self.config.bar.margin
            || cfg.bar.layer != self.config.bar.layer;
        self.config = cfg;
        self.theme = self.config.theme_tokens();
        // Modules reconcile by key (idempotent: unchanged in ⇒ no churn out).
        let modules = self.reconcile_modules();
        // `[bar].outputs` may have changed — add/drop surfaces to match.
        let surfaces = self.reconcile_surfaces();
        let geom = if geom_changed {
            self.reconcile_bar_geometry(self.config.bar.position)
        } else {
            Task::none()
        };
        Task::batch([modules, surfaces, geom])
    }

    /// RFC 0004: re-anchor / re-size / re-layer the *live* bar surface to match the
    /// current config (at position `pos`) by emitting iced_layershell's in-place
    /// mutation messages — no surface re-roll, no exit-on-close dance. The change
    /// messages are consumed by the layershell runtime (not our `update`). Updates
    /// `bar_pos` so a later diff is a no-op.
    fn reconcile_bar_geometry(&mut self, pos: config::Position) -> Task<Message> {
        let g = bar_geom(&self.config.bar, pos);
        self.bar_pos = pos;
        let mut tasks = Vec::with_capacity(self.bars.len() * 4);
        for b in &self.bars {
            let id = b.id;
            // Each mutator commits the surface itself, so order to minimise visible
            // intermediate states: the non-reflowing attributes (layer, margin,
            // exclusive zone) first, then anchor+size *together* and *last* via
            // AnchorSizeChange (one set_anchor+set_size+commit). The compositor then
            // reflows tiled windows at most once, with margins/zone already correct —
            // instead of up to four times across five separate commits.
            tasks.push(Task::done(Message::LayerChange { id, layer: g.layer }));
            tasks.push(Task::done(Message::MarginChange {
                id,
                margin: g.margin,
            }));
            tasks.push(Task::done(Message::ExclusiveZoneChange {
                id,
                zone_size: g.exclusive_zone,
            }));
            tasks.push(Task::done(Message::AnchorSizeChange {
                id,
                anchor: g.anchor,
                size: g.size,
            }));
        }
        Task::batch(tasks)
    }

    /// RFC 0004: reconcile the bar's *surface set* against the outputs matching
    /// `[bar].outputs`. Idempotent: opens a surface for a newly-matching output,
    /// closes one whose output vanished or no longer matches, and refreshes kept
    /// surfaces' cached width. Driven by startup, config reload, output hotplug,
    /// and a bar surface closing. This — not re-roll — is the only create/destroy
    /// path; geometry-only changes mutate live surfaces in place.
    fn reconcile_surfaces(&mut self) -> Task<Message> {
        let desired = desired_outputs(&self.config);
        let desired_names: std::collections::HashSet<&str> =
            desired.iter().map(|o| o.name.as_str()).collect();
        log::info!(
            "reconcile: desired={:?} tracked={:?}",
            desired_names,
            self.bars
                .iter()
                .map(|b| (b.output.as_str(), b.id))
                .collect::<Vec<_>>()
        );
        let mut tasks = Vec::new();

        // The pure close/create decision (deduped against the two-bars saga).
        let desired_list: Vec<String> = desired.iter().map(|o| o.name.clone()).collect();
        let tracked: Vec<(String, window::Id)> =
            self.bars.iter().map(|b| (b.output.clone(), b.id)).collect();
        let (to_close, to_create) = plan_surfaces(&desired_list, &tracked);

        // Close surfaces whose output is gone or de-selected.
        let closed_any = !to_close.is_empty();
        for id in &to_close {
            log::info!("reconcile: CLOSE {id:?} (output de-selected)");
            tasks.push(iced::window::close(*id));
        }
        self.bars.retain(|b| !to_close.contains(&b.id));

        // Refresh widths of surviving surfaces against the live output geometry.
        for o in &desired {
            if let Some(b) = self.bars.iter_mut().find(|b| b.output == o.name) {
                b.width = o.width;
            }
        }

        // Open surfaces for newly-matching outputs.
        for name in to_create {
            let width = desired.iter().find(|o| o.name == name).map_or(1920, |o| o.width);
            let id = window::Id::unique();
            log::info!("reconcile: CREATE {id:?} for output {name}");
            tasks.push(Task::done(Message::NewLayerShell {
                settings: bar_settings(&self.config, self.bar_pos, &name),
                id,
            }));
            self.bars.push(BarSurface { id, output: name, width });
        }

        // If the cursor's output no longer has a bar, forget it (so popups don't
        // target a dead output) and re-base the popup-clamp width on a survivor.
        if let Some(name) = &self.cursor_output {
            if !self.bars.iter().any(|b| &b.output == name) {
                self.cursor_output = None;
                self.screen_w = self.bars.first().map(|b| b.width).unwrap_or(1920);
            }
        }
        // An output went away — close any transient popup (it lived on a surface
        // that may be gone; the compositor would close it anyway, but don't leave
        // dangling popup state pointing at a dead output).
        if closed_any {
            tasks.push(self.close_any_popup());
        }
        Task::batch(tasks)
    }

    /// RFC 0004: reconcile the live module set against config, keyed by `key`.
    /// **Unchanged** instances keep their state, recipes, and running streams;
    /// **added** are constructed; **removed** are shut down; **config-changed** are
    /// `reconfigure`d (or rebuilt) with a generation bump that re-keys their
    /// subscriptions. Order follows placement. Returns a task to close a module
    /// popup whose owning instance went away.
    fn reconcile_modules(&mut self) -> Task<Message> {
        let rt = self.rt.clone(); // cheap (Arc); avoids borrowing self in the build calls
        let specs = desired_module_specs(&self.config);
        let mut live: HashMap<String, ModuleEntry> = self
            .modules
            .drain(..)
            .map(|e| (e.name.clone(), e))
            .collect();
        let mut next: Vec<ModuleEntry> = Vec::with_capacity(specs.len());
        for s in specs {
            let id = stable_id(&s.key);
            match live.remove(&s.key) {
                // unchanged: keep the instance, its state, and its subscriptions.
                Some(entry) if entry.cfg == s.cfg => next.push(entry),
                // same key, changed config: adopt in place or rebuild.
                Some(mut entry) => match entry.module.reconfigure(&s.cfg) {
                    Reconfigure::Applied { resubscribe } => {
                        entry.cfg = s.cfg;
                        if resubscribe {
                            self.bump_generation(id);
                        }
                        next.push(entry);
                    }
                    Reconfigure::Reconstruct => {
                        entry.module.shutdown();
                        if let Some(m) = modules::build(&s.type_id, id, &s.cfg, &rt) {
                            self.bump_generation(id);
                            next.push(ModuleEntry::new(id, s.key, m, s.cfg));
                        }
                    }
                },
                // added. Bump generation unconditionally so the new instance's
                // recipe key is fresh — startup-built instances run at generation 0
                // without ever being recorded in the map, so a remove→re-add must
                // step past 0 to avoid reusing the old (id, 0) recipe key.
                None => {
                    if let Some(m) = modules::build(&s.type_id, id, &s.cfg, &rt) {
                        self.bump_generation(id);
                        next.push(ModuleEntry::new(id, s.key, m, s.cfg));
                    }
                }
            }
        }
        // Whatever is left was removed from placement. Keep its `generation` entry
        // (monotonic per id) so a later re-add re-keys past any draining recipe.
        for (_, mut e) in live.drain() {
            e.module.shutdown();
        }
        self.modules = next;
        // A module popup whose owner no longer exists must close.
        if let Some((pid, inst, _)) = self.module_popup {
            if !self.modules.iter().any(|e| e.id == inst) {
                self.module_popup = None;
                return iced::window::close(pid);
            }
        }
        Task::none()
    }

    /// Bump an instance's subscription generation so iced re-rolls its recipes.
    fn bump_generation(&mut self, id: u64) {
        *self.generation.entry(id).or_insert(0) += 1;
    }

    /// Map an `ezbar msg` command line to an action (RFC 0002 IPC).
    fn handle_ipc(&mut self, cmd: &str) -> Task<Message> {
        let parts: Vec<&str> = cmd.split_whitespace().collect();
        match parts.as_slice() {
            ["reload"] => self.apply_config(config::load()),
            ["preset", dir @ ("next" | "prev")] => {
                let names = config::preset_names();
                if names.is_empty() {
                    return Task::none();
                }
                let cur = config::active_preset();
                let idx = cur
                    .as_deref()
                    .and_then(|c| names.iter().position(|n| n == c))
                    .unwrap_or(0);
                let n = names.len();
                let next = if *dir == "next" {
                    (idx + 1) % n
                } else {
                    (idx + n - 1) % n
                };
                Task::done(Message::SelectPreset(names[next].clone()))
            }
            ["preset", name] => Task::done(Message::SelectPreset((*name).to_string())),
            ["popup", kind] | ["popup", "toggle", kind] => match parse_popup_kind(kind) {
                Some(k) => Task::done(Message::OpenPopup(k)),
                None => {
                    log::warn!("ipc: unknown popup '{kind}'");
                    Task::none()
                }
            },
            ["volume", "up"] => Task::done(Message::VolumeAdjust(1)),
            ["volume", "down"] => Task::done(Message::VolumeAdjust(-1)),
            ["volume", "mute"] => Task::done(Message::VolumeAdjust(0)),
            // PoC (RFC 0004): re-anchor the live bar surface in place.
            ["position", p @ ("top" | "bottom" | "toggle")] => {
                let next = match *p {
                    "top" => config::Position::Top,
                    "bottom" => config::Position::Bottom,
                    _ => match self.bar_pos {
                        config::Position::Top => config::Position::Bottom,
                        config::Position::Bottom => config::Position::Top,
                    },
                };
                self.reconcile_bar_geometry(next)
            }
            _ => {
                log::warn!("ipc: unknown command: {cmd:?}");
                Task::none()
            }
        }
    }

    fn style(&self, _theme: &iced::Theme) -> iced::theme::Style {
        let bg = self.config.theme.background.base().0;
        // Islands draw their own pills over a transparent surface; solid paints
        // the whole bar background.
        let bar_bg = match self.config.theme.style {
            Style::Islands => Color::TRANSPARENT,
            Style::Solid => Color::from_rgba(bg[0], bg[1], bg[2], self.config.theme.opacity),
        };
        iced::theme::Style {
            background_color: bar_bg,
            text_color: self.config.theme.text.iced(),
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        let mut subs = vec![
            Subscription::run(config_stream),
            Subscription::run(ipc_stream),
            Subscription::run(outputs_stream),
            event::listen_with(|ev, _status, id| match ev {
                iced::Event::Window(iced::window::Event::Closed) => Some(Message::WindowClosed(id)),
                iced::Event::Mouse(iced::mouse::Event::CursorMoved { position }) => {
                    Some(Message::Cursor(id, position.x))
                }
                _ => None,
            }),
        ];
        // Pluggable modules contribute their own subscriptions. The host owns
        // instance-keying via `.with(instance)`: it both routes the message
        // (injecting the instance id without a capturing `map` closure, which
        // Subscription::map forbids) AND makes two instances of the same module
        // produce distinct recipes. Modules need not key by instance themselves.
        for entry in &self.modules {
            if entry.disabled {
                continue;
            }
            let id = entry.id;
            let generation = self.generation.get(&id).copied().unwrap_or(0);
            subs.push(
                entry
                    .module
                    .subscription()
                    .with((id, generation))
                    .map(|((instance, _gen), m)| Message::ModuleMsg { instance, msg: m }),
            );
        }
        Subscription::batch(subs)
    }
}

// ---- subscription streams (one per data source) ----

/// Re-run the full load pipeline (config.toml + drop-in presets + active preset),
/// so a file-watch reload keeps the user's selected preset (not just inline theme).
fn read_parse(_path: &std::path::Path) -> Result<Config, String> {
    // On a *reload*, a missing/unreadable config means "keep the current config", NOT "reset
    // to defaults" — so the live bar never flashes to defaults during an editor's atomic save
    // (write-temp + rename leaves the path briefly absent) or while the file is transiently
    // gone. This matches the parse-error path (both → `Err` → keep-last-good). Startup
    // `config::load()` still treats a missing config as defaults, which is right for a fresh
    // install; only the reload path is conservative.
    match config::path() {
        Some(p) if p.exists() => config::load_result(),
        Some(_) => Err("config file missing — keeping current".into()),
        None => Err("no config path".into()),
    }
}

/// Subscribe to sway `output` events and emit [`Message::OutputsChanged`] on each,
/// so the host reconciles its per-output surface set on monitor hotplug/layout
/// changes (RFC 0004). The blocking sway event iterator runs on a blocking task and
/// forwards through a channel; the connection is re-established with a backoff if it
/// drops (e.g. sway restart). Output naming/IPC is sway-specific by design — ezbar
/// is a sway bar — but the host-side reconcile is compositor-agnostic.
fn outputs_stream() -> impl Stream<Item = Message> {
    iced::stream::channel(
        4,
        |mut out: iced::futures::channel::mpsc::Sender<Message>| async move {
            loop {
                let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(8);
                let handle = tokio::task::spawn_blocking(move || {
                    let events = match swayipc::Connection::new()
                        .and_then(|c| c.subscribe([swayipc::EventType::Output]))
                    {
                        Ok(e) => e,
                        Err(e) => {
                            log::warn!("outputs: sway subscribe: {e}");
                            return;
                        }
                    };
                    // A fresh `subscribe` only delivers *deltas*, so kick one
                    // reconcile against current reality on every (re)connect — else
                    // a cold start where sway became reachable after launch, or a
                    // sway restart with unchanged outputs, would leave the bar with
                    // whatever surfaces it had (possibly none) until the next change.
                    if tx.blocking_send(()).is_err() {
                        return;
                    }
                    for ev in events {
                        match ev {
                            Ok(swayipc::Event::Output(_)) => {
                                if tx.blocking_send(()).is_err() {
                                    break;
                                }
                            }
                            Ok(_) => {}
                            Err(e) => {
                                log::warn!("outputs: sway event: {e}");
                                break;
                            }
                        }
                    }
                });
                // Forward each output change until the sway event stream ends,
                // coalescing bursts and letting the change settle first. The settle
                // delay matters: sway's Output IPC event can arrive before the
                // matching `wl_output` global reaches iced_layershell's cache, and a
                // `NewLayerShell{OutputName}` that the cache can't yet resolve binds
                // to the compositor-default output instead. Waiting lets the global
                // land so the bind targets the intended output. (The true fix is an
                // upstream `Output(wl_output)` / bind-result API — see RFC 0004.)
                while rx.recv().await.is_some() {
                    while rx.try_recv().is_ok() {}
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    while rx.try_recv().is_ok() {}
                    if out.send(Message::OutputsChanged).await.is_err() {
                        return;
                    }
                }
                let _ = handle.await;
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        },
    )
}

/// Watch the config directory; emit a reloaded (or errored) config on change.
/// Keep-last-good: a parse error yields `Err` and the host keeps the running config.
fn config_stream() -> impl Stream<Item = Message> {
    iced::stream::channel(
        4,
        |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
            use notify::Watcher;
            let Some(path) = config::path() else {
                return;
            };
            let Some(dir) = path.parent().map(|p| p.to_path_buf()) else {
                return;
            };
            let _ = std::fs::create_dir_all(&dir);

            let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(16);
            let mut watcher =
                match notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                    if res.is_ok() {
                        let _ = tx.blocking_send(());
                    }
                }) {
                    Ok(w) => w,
                    Err(e) => {
                        log::warn!("config watch: {e}");
                        return;
                    }
                };
            if let Err(e) = watcher.watch(&dir, notify::RecursiveMode::NonRecursive) {
                log::warn!("config watch: {e}");
                return;
            }
            loop {
                if rx.recv().await.is_none() {
                    break;
                }
                // debounce: coalesce a burst of fs events
                tokio::time::sleep(Duration::from_millis(150)).await;
                while rx.try_recv().is_ok() {}
                // read + parse; retry once on error (likely a mid-write read)
                let mut cfg = read_parse(&path);
                if cfg.is_err() {
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    cfg = read_parse(&path);
                }
                if output.send(Message::ConfigReloaded(cfg)).await.is_err() {
                    break;
                }
            }
            drop(watcher);
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(cfg: &Config) -> Vec<String> {
        desired_module_specs(cfg).into_iter().map(|s| s.key).collect()
    }

    // ── output-churn reconcile (RFC 0004) — the regression harness for the two-bars saga ──
    fn plan(desired: &[&str], tracked: &[(&str, u32)]) -> (Vec<u32>, Vec<String>) {
        let d: Vec<String> = desired.iter().map(|s| s.to_string()).collect();
        let t: Vec<(String, u32)> = tracked.iter().map(|(n, i)| (n.to_string(), *i)).collect();
        plan_surfaces(&d, &t)
    }

    #[test]
    fn churn_creates_appearing_outputs_and_closes_gone_ones() {
        // new output B appears next to a tracked A
        assert_eq!(plan(&["DP-1", "DP-2"], &[("DP-1", 1)]), (vec![], vec!["DP-2".into()]));
        // an output goes away → its surface closes
        assert_eq!(plan(&["DP-1"], &[("DP-1", 1), ("DP-2", 2)]), (vec![2], vec![]));
        // hot-swap: A leaves as B arrives
        assert_eq!(plan(&["DP-2"], &[("DP-1", 1)]), (vec![1], vec!["DP-2".into()]));
        // steady state: nothing to do (idempotent — no churn out for unchanged in)
        assert_eq!(plan(&["DP-1"], &[("DP-1", 1)]), (vec![], vec![]));
    }

    #[test]
    fn churn_never_stacks_a_duplicate_bar() {
        // the two-bars saga: the same output listed twice must create ONE surface, not two
        assert_eq!(plan(&["DP-1", "DP-1"], &[]), (vec![], vec!["DP-1".into()]));
        // and never a second surface for an already-tracked output (even if double-listed)
        assert_eq!(plan(&["DP-1", "DP-1"], &[("DP-1", 1)]), (vec![], vec![]));
    }

    #[test]
    fn default_placement_resolves_expected_module_set() {
        let cfg = config::parse_str("").unwrap();
        let ks = keys(&cfg);
        // left zone defaults to workspaces (leads); clock anchors the far-right end-cap.
        assert_eq!(ks.first().map(String::as_str), Some("workspaces"));
        assert_eq!(ks.last().map(String::as_str), Some("clock"));
        // host chrome (the ▾ switcher) is never resolved as a module…
        assert!(!ks.iter().any(|k| k == "switcher"));
        // …and every resolved spec IS a real module (no chrome leaks through).
        assert!(desired_module_specs(&cfg)
            .iter()
            .all(|s| modules::is_module(&s.type_id)));
    }

    #[test]
    fn duplicate_keys_are_deduped() {
        // the same key placed twice yields exactly one instance (reconcile identity).
        let cfg = config::parse_str("left = [\"cpu\", \"cpu\"]").unwrap();
        assert_eq!(keys(&cfg).iter().filter(|k| *k == "cpu").count(), 1);
    }

    #[test]
    fn explicit_left_zone_overrides_the_default() {
        // an explicit left zone replaces the workspaces default, in order.
        let cfg = config::parse_str("left = [\"clock\", \"cpu\"]").unwrap();
        let ks = keys(&cfg);
        assert_eq!(ks.first().map(String::as_str), Some("clock"));
        assert!(ks.iter().any(|k| k == "cpu"));
        assert!(!ks.iter().any(|k| k == "workspaces")); // default no longer injected
    }

    #[test]
    fn non_module_chrome_in_a_zone_is_skipped() {
        // placing the switcher explicitly must not produce a module spec for it.
        let cfg = config::parse_str("center = [\"switcher\"]").unwrap();
        assert!(!keys(&cfg).iter().any(|k| k == "switcher"));
    }

    #[test]
    fn stable_id_is_deterministic_and_distinct() {
        // Same key → same id across calls (so a reconcile matches instances).
        assert_eq!(stable_id("clock"), stable_id("clock"));
        assert_eq!(stable_id("stock:nasdaq"), stable_id("stock:nasdaq"));
        // Different keys → different ids (no accidental aliasing of instances).
        assert_ne!(stable_id("cpu"), stable_id("clock"));
        assert_ne!(stable_id("stock:nasdaq"), stable_id("stock:dax"));
    }

    #[test]
    fn desired_specs_dedup_order_and_skip_chrome() {
        let cfg = Config::default();
        let specs = desired_module_specs(&cfg);
        // Default placement renders the shipped modules; keys are unique.
        let keys: Vec<&str> = specs.iter().map(|s| s.key.as_str()).collect();
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), keys.len(), "duplicate instance key in specs");
        // Left zone first → workspaces leads; the right cluster includes clock.
        assert_eq!(keys.first(), Some(&"workspaces"));
        assert!(keys.contains(&"clock"));
        // Host chrome (the `switcher`) is never a module instance.
        assert!(!keys.contains(&"switcher"));
        // Every spec names a real module type.
        assert!(specs.iter().all(|s| modules::is_module(&s.type_id)));
    }
}
