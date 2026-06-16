//! Render-preview a WASM plugin — the author's "does my chip actually render?"
//! loop, without launching the whole bar. Works for plugins written in ANY
//! language that compiles to a wasip2 component (Rust, Go/TinyGo, …).
//!
//!     cargo run -p ezbar-wasm --example preview -- <path-to.wasm> \
//!         [--net <host>]... [--set <key>=<value>]... [--check]
//!
//! It loads the component with the real host runtime ([`WasmModule`]) and drives
//! it through the same harness the bar uses (subscription → update → view →
//! popup), so the chip, colours, and hover popup look exactly as they will live.
//! `--net` grants a network host (the bar's `[modules.<id>].network`); `--set
//! k=v` feeds `[modules.<id>]` config to the plugin's `load`.
//!
//! `--check` is a HEADLESS smoke test (no window, CI-friendly): it drives the
//! plugin for a moment and reports the rendered node counts, exiting non-zero if
//! nothing rendered (e.g. the plugin trapped). Plugin traps/log lines are printed
//! to stderr in both modes.
//!
//! Example — preview the weather plugin with live data:
//!     cargo run -p ezbar-wasm --example preview -- \
//!         ../../wasm/weather/target/wasm32-wasip2/release/weather.wasm \
//!         --net api.open-meteo.com --set lat=48.13 --set lon=11.57

use std::path::PathBuf;
use std::process::exit;
use std::time::{Duration, Instant};

use ezbar_wasm::WasmModule;

fn main() -> ezbar_plugin::iced::Result {
    install_logger();

    let mut args = std::env::args().skip(1);
    let mut path: Option<PathBuf> = None;
    let mut grants: Vec<String> = Vec::new();
    let mut grants_feeds: Vec<String> = Vec::new();
    let mut grant_sway = false;
    let mut grants_fs: Vec<ezbar_wasm::FsGrant> = Vec::new();
    let mut grants_exec: Vec<String> = Vec::new();
    let mut config: Vec<(String, String)> = Vec::new();
    let mut mem_limit: usize = ezbar_wasm::MEM_LIMIT;
    let mut check = false;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--net" => match args.next() {
                Some(host) => grants.push(host),
                None => fail("--net needs a host, e.g. --net api.open-meteo.com"),
            },
            "--feed" => match args.next() {
                Some(kind) => grants_feeds.push(kind),
                None => fail("--feed needs a kind, e.g. --feed cpu"),
            },
            "--sway" => grant_sway = true,
            // --fs <hostpath>[:<guestmount>][:rw]   (mount defaults to /<basename>, ro)
            "--fs" => match args.next() {
                Some(spec) => {
                    let mut it = spec.split(':');
                    let host = it.next().unwrap_or("");
                    let host_path = PathBuf::from(host);
                    let guest_path = match it.next().filter(|s| !s.is_empty()) {
                        Some(g) => g.to_string(),
                        None => format!(
                            "/{}",
                            host_path
                                .file_name()
                                .and_then(|s| s.to_str())
                                .unwrap_or("dir")
                        ),
                    };
                    let write = it.next() == Some("rw");
                    grants_fs.push(ezbar_wasm::FsGrant {
                        host_path,
                        guest_path,
                        write,
                    });
                }
                None => fail("--fs needs <hostpath>[:<guestmount>][:rw]"),
            },
            "--exec" => match args.next() {
                Some(prog) => grants_exec.push(prog),
                None => fail("--exec needs a program, e.g. --exec echo"),
            },
            "--set" => match args.next() {
                Some(kv) => match kv.split_once('=') {
                    Some((k, v)) => config.push((k.to_string(), v.to_string())),
                    None => fail("--set needs key=value, e.g. --set lat=52.52"),
                },
                None => fail("--set needs key=value, e.g. --set lat=52.52"),
            },
            // --max-memory <n>[K|M|G] — raise the plugin's linear-memory cap (mirrors
            // `[modules.<id>].max_memory`); needed to preview a plugin that holds a big payload.
            "--max-memory" => match args.next() {
                Some(s) => mem_limit = parse_mem(&s),
                None => fail("--max-memory needs a size, e.g. --max-memory 48M"),
            },
            "--check" => check = true,
            "-h" | "--help" => usage(0),
            _ if path.is_none() => path = Some(PathBuf::from(arg)),
            other => fail(&format!("unexpected argument: {other}")),
        }
    }

    let Some(path) = path else { usage(2) };
    if !path.exists() {
        fail(&format!("no such file: {}", path.display()));
    }
    // the placement id a plugin sees is the .wasm file stem (as in the bar).
    let id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("plugin")
        .to_string();

    eprintln!(
        "preview: {} (id={id}, net={:?}, config={:?})",
        path.display(),
        grants,
        config
    );
    // The reactor drives the plugin on a tokio runtime. The bar reuses iced's; the
    // preview owns a small one (RFC 0008 §3.1) — kept alive for the whole run, so it
    // keeps driving the plugin task even while the harness window blocks below, and
    // so headless `--check` (which never starts iced) has a runtime at all.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("preview: build tokio runtime");
    // A stub sway source so `--sway` plugins have something to read in the preview (the real
    // bar injects its live `sources::sway` snapshot; the harness fakes one).
    if grant_sway {
        ezbar_wasm::set_sway_source(std::sync::Arc::new(|| ezbar_wasm::SwaySnapshot {
            workspaces: vec![
                ezbar_wasm::SwayWorkspaceInfo {
                    name: "1".into(),
                    focused: true,
                    visible: true,
                    urgent: false,
                },
                ezbar_wasm::SwayWorkspaceInfo {
                    name: "2".into(),
                    focused: false,
                    visible: false,
                    urgent: false,
                },
            ],
            title: "preview — focused window title".into(),
        }));
    }
    let module = WasmModule::new(
        rt.handle().clone(),
        0,
        id,
        path,
        config,
        grants,
        grants_feeds,
        grant_sway,
        grants_fs,
        grants_exec,
        mem_limit,
    );

    // Headless smoke test: drive the plugin briefly and report what it rendered.
    if check {
        return run_check(&module);
    }

    // A window needs a display. On a headless box, say so plainly instead of
    // letting iced panic deep in winit — the line above already confirms the
    // component loaded and its capabilities/config wired up. (Use --check to
    // actually verify rendering headlessly.)
    if std::env::var_os("WAYLAND_DISPLAY").is_none() && std::env::var_os("DISPLAY").is_none() {
        eprintln!(
            "preview: no display (WAYLAND_DISPLAY / DISPLAY unset) — component loaded fine; \
run this on your graphical session to see the chip, or pass --check to verify headlessly."
        );
        return Ok(());
    }
    ezbar_harness::run(Box::new(module))
}

/// Poll the actor's rendered snapshot until the chip has nodes (or we give up),
/// then report. Exits non-zero if nothing rendered — a CI-usable assertion.
fn run_check(module: &WasmModule) -> ezbar_plugin::iced::Result {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let (view, popup) = module.debug_snapshot();
        if view > 0 {
            eprintln!("preview: OK — chip rendered {view} node(s), popup {popup} node(s).");
            return Ok(());
        }
        if Instant::now() >= deadline {
            eprintln!(
                "preview: FAIL — the plugin produced no chip within 8s. It may have trapped \
(see warnings above), be waiting on a denied capability (pass --net), or its `view` returned \
nothing. "
            );
            exit(1);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// A tiny stderr logger so plugin traps / `ctx.log` lines surface during preview
/// (the actor logs via the `log` crate; without this they'd be swallowed).
fn install_logger() {
    use log::{Level, LevelFilter, Metadata, Record};
    struct Stderr;
    impl log::Log for Stderr {
        fn enabled(&self, m: &Metadata) -> bool {
            m.level() <= Level::Info
        }
        fn log(&self, r: &Record) {
            if self.enabled(r.metadata()) {
                eprintln!("[{}] {}", r.level().to_string().to_lowercase(), r.args());
            }
        }
        fn flush(&self) {}
    }
    let _ = log::set_boxed_logger(Box::new(Stderr));
    log::set_max_level(LevelFilter::Info);
}

fn usage(code: i32) -> ! {
    eprintln!(
        "usage: cargo run -p ezbar-wasm --example preview -- \
<path-to.wasm> [--net <host>]... [--set <key>=<value>]... [--check]"
    );
    exit(code)
}

fn fail(msg: &str) -> ! {
    eprintln!("preview: {msg}");
    exit(2)
}

/// Parse a `--max-memory` size like `48`, `48M`, `48MiB` into bytes (K/M/G are powers of 1024).
fn parse_mem(s: &str) -> usize {
    let t = s.trim();
    let t = t
        .strip_suffix("iB")
        .or_else(|| t.strip_suffix('B'))
        .unwrap_or(t)
        .trim();
    let (digits, mult) = match t.chars().last() {
        Some('K' | 'k') => (&t[..t.len() - 1], 1usize << 10),
        Some('M' | 'm') => (&t[..t.len() - 1], 1usize << 20),
        Some('G' | 'g') => (&t[..t.len() - 1], 1usize << 30),
        _ => (t, 1usize),
    };
    match digits.trim().parse::<usize>() {
        Ok(n) => n.saturating_mul(mult).max(ezbar_wasm::MEM_LIMIT),
        Err(_) => fail("--max-memory: bad size (try 48M)"),
    }
}
