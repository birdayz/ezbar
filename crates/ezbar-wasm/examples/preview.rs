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
    let mut config: Vec<(String, String)> = Vec::new();
    let mut check = false;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--net" => match args.next() {
                Some(host) => grants.push(host),
                None => fail("--net needs a host, e.g. --net api.open-meteo.com"),
            },
            "--set" => match args.next() {
                Some(kv) => match kv.split_once('=') {
                    Some((k, v)) => config.push((k.to_string(), v.to_string())),
                    None => fail("--set needs key=value, e.g. --set lat=52.52"),
                },
                None => fail("--set needs key=value, e.g. --set lat=52.52"),
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
    let module = WasmModule::new(0, id, path, config, grants);

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
