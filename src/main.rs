use std::time::Duration;

use iced::alignment::{Horizontal, Vertical};
use iced::futures::{SinkExt, Stream};
use iced::widget::{button, column, container, mouse_area, row, scrollable, text, Space};
use iced::{event, window, Background, Border, Color, Element, Length, Subscription, Task};
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

mod install;
mod ipc;

const BAR_HEIGHT: u32 = 34;

/// Default right-zone placement (today's bar) when `right` is unconfigured.
const DEFAULT_RIGHT: &[&str] = &[
    "cpu",
    "github",
    "claude",
    "calendar",
    "kubectl",
    "temperature",
    "memory",
    "ping",
    "spotify",
    "stock",
    "volume",
    "battery",
    "clock",
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
         ezbar --version    print the version\n    \
         ezbar --help       print this help\n\n\
         EZBAR_CHILD=1 ezbar   run a single foreground instance (no respawn)"
    );
}

fn run_bar() -> iced_layershell::Result {
    // Default to a Nerd Font so the icon glyphs render; overridable via [bar].font.
    let cfg = config::load();
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
        .default_text_size(14.0)
        .default_font(font)
        .run()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PopupKind {
    /// the ▾ preset switcher — the only host-owned popup; module popups (calendar,
    /// stock, kubectl) go through the module-popup path (`HostRequest::OpenPopup`).
    Switcher,
}

struct Bar {
    bar_id: window::Id,
    popup: Option<(window::Id, PopupKind)>,
    module_popup: Option<(window::Id, u64, PopupMode)>,
    modules: Vec<ModuleEntry>,

    // popup anchoring: last cursor x over the bar, so a popup opens above the
    // widget the user interacted with (RFC 0001 slot-derived).
    cursor_x: f32,

    // config + resolved module theme (RFC 0002)
    config: Config,
    theme: ThemeTokens,

    // focused output width, so popups can be clamped on-screen
    screen_w: u32,

    // RFC 0004: the live bar surface's current geometry position, so a reconcile
    // can diff against it and re-anchor in place instead of re-rolling.
    bar_pos: config::Position,

    // RFC 0002/0004: per-instance subscription generation. Bumped when a module is
    // reconfigured/reconstructed so the host's `.with((id, gen))` recipe key changes
    // and iced re-rolls that instance's streams (the old config's streams drop).
    generation: HashMap<u64, u64>,
}

/// Width of the focused sway output (for clamping popups). Falls back to 1920.
fn focused_output_width() -> u32 {
    swayipc::Connection::new()
        .ok()
        .and_then(|mut c| c.get_outputs().ok())
        .and_then(|outs| {
            outs.iter()
                .find(|o| o.focused)
                .or_else(|| outs.first())
                .map(|o| o.rect.width as u32)
        })
        .filter(|w| *w > 0)
        .unwrap_or(1920)
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
    Noop,
    ModuleMsg { instance: u64, msg: ModMsg },
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

fn bar_settings(cfg: &Config) -> NewLayerShellSettings {
    let g = bar_geom(&cfg.bar, cfg.bar.position);
    NewLayerShellSettings {
        size: Some(g.size),
        exclusive_zone: Some(g.exclusive_zone),
        anchor: g.anchor,
        margin: Some(g.margin),
        layer: g.layer,
        keyboard_interactivity: KeyboardInteractivity::None,
        output_option: OutputOption::None,
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

/// All placed items across the three zones, in order (zones fall back to defaults).
fn all_placed(config: &Config) -> Vec<Placed> {
    let mut items = Vec::new();
    resolve_zone(&config.left, &["workspaces"], &mut items);
    resolve_zone(&config.center, &["window_title"], &mut items);
    resolve_zone(&config.right, DEFAULT_RIGHT, &mut items);
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
fn build_modules(config: &Config) -> Vec<ModuleEntry> {
    desired_module_specs(config)
        .into_iter()
        .filter_map(|s| {
            let id = stable_id(&s.key);
            modules::build(&s.type_id, id, &s.cfg).map(|m| ModuleEntry::new(id, s.key, m, s.cfg))
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
            let _ = std::fs::remove_file(&path); // clear any stale socket
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

/// Layer-shell settings for a popup floating above the bar, with its left edge
/// at `left_margin`. The host derives `left_margin` from the cursor so the popup
/// sits over the widget the user interacted with (RFC 0001, slot-derived).
fn popup_layer_settings(
    size: (u32, u32),
    left_margin: i32,
    events_transparent: bool,
) -> NewLayerShellSettings {
    NewLayerShellSettings {
        size: Some(size),
        exclusive_zone: None,
        anchor: Anchor::Bottom | Anchor::Left,
        layer: Layer::Overlay,
        // margin order: (top, right, bottom, left); float above the bar.
        margin: Some((0, 0, BAR_HEIGHT as i32 + 6, left_margin)),
        keyboard_interactivity: KeyboardInteractivity::None,
        events_transparent,
        output_option: OutputOption::None,
        namespace: Some("ezbar-popup".to_string()),
    }
}

impl Bar {
    fn new() -> (Self, Task<Message>) {
        let bar_id = window::Id::unique();
        let config = config::load();
        let open = Task::done(Message::NewLayerShell {
            settings: bar_settings(&config),
            id: bar_id,
        });
        let theme = config.theme_tokens();
        let bar_pos = config.bar.position;
        let bar = Bar {
            bar_id,
            popup: None,
            module_popup: None,
            modules: build_modules(&config),
            cursor_x: 0.0,
            config,
            theme,
            screen_w: focused_output_width(),
            bar_pos,
            generation: HashMap::new(),
        };
        // Dev/screenshot hook: open the switcher popup on startup for capture.
        if std::env::var("EZBAR_OPEN_POPUP").is_ok() {
            return (
                bar,
                Task::batch([open, Task::done(Message::OpenPopup(PopupKind::Switcher))]),
            );
        }
        (bar, open)
    }

    fn namespace() -> String {
        "ezbar".to_string()
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::VolumeAdjust(dir) => Task::perform(
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
            ),
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
                if id == self.bar_id {
                    self.cursor_x = x;
                }
                Task::none()
            }
            Message::WindowClosed(id) => {
                if id == self.bar_id {
                    // Bar surface gone (e.g. monitor unplugged/slept) — exit so the
                    // launcher respawns us and re-binds when the output returns.
                    return iced::exit();
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
                let left = self.popup_left_margin(MODULE_POPUP_SIZE.0);
                let open = Task::done(Message::NewLayerShell {
                    settings: popup_layer_settings(
                        MODULE_POPUP_SIZE,
                        left,
                        matches!(mode, PopupMode::Hover),
                    ),
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

    fn open_popup(&self, kind: PopupKind) -> (window::Id, Task<Message>) {
        let id = window::Id::unique();
        let size = popup_size(kind);
        let left = self.popup_left_margin(size.0);
        (
            id,
            Task::done(Message::NewLayerShell {
                settings: popup_layer_settings(size, left, false),
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
        if id == self.bar_id {
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
        let dim = self.config.theme.dim.iced();
        mouse_area(
            container(
                text("\u{f107}")
                    .size(self.config.theme.font_size)
                    .color(dim),
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

    /// Build one zone from ordered `Placed` items: a module instance renders by `key`,
    /// host chrome renders by `type_id`. A configured separator goes between items (never
    /// before the `▾` switcher).
    fn build_zone(&self, items: &[Placed]) -> Element<'_, Message> {
        let s = &self.config.theme.separator;
        let glyph = s.glyph.clone().unwrap_or_else(|| "|".to_string());
        let sep_color = s.color.iced();
        let mut out: Vec<Element<Message>> = Vec::new();
        for p in items {
            // A built module instance is keyed by `key`; otherwise try host chrome.
            let el = if self.modules.iter().any(|e| e.name == p.key) {
                self.render_module(&p.key)
            } else {
                self.render_widget(&p.type_id)
            };
            if let Some(el) = el {
                if !out.is_empty() && p.type_id != "switcher" {
                    out.push(text(glyph.clone()).color(sep_color).into());
                }
                out.push(el);
            }
        }
        row(out).spacing(6).align_y(Vertical::Center).into()
    }

    fn bar_view(&self) -> Element<'_, Message> {
        // Config placement (RFC 0002) drives which widgets render and in what order;
        // an empty zone falls back to the shipped default (today's bar).
        let mut left = Vec::new();
        let mut center = Vec::new();
        let mut right = Vec::new();
        resolve_zone(&self.config.left, &["workspaces"], &mut left);
        resolve_zone(&self.config.center, &["window_title"], &mut center);
        resolve_zone(&self.config.right, DEFAULT_RIGHT, &mut right);

        // Place the ▾ switcher per [bar].switcher (unless the user listed it).
        let switcher = || Placed {
            key: "switcher".into(),
            type_id: "switcher".into(),
            config: empty_cfg(),
        };
        let has_switcher = |v: &[Placed]| v.iter().any(|p| p.type_id == "switcher");
        match self.config.bar.switcher {
            SwitcherPos::Left if !has_switcher(&left) => left.insert(0, switcher()),
            SwitcherPos::Right if !has_switcher(&right) => right.push(switcher()),
            _ => {}
        }

        let ws_row = self.build_zone(&left);
        let title_el = self.build_zone(&center);
        let right_inner = self.build_zone(&right);

        if matches!(self.config.theme.style, Style::Islands) {
            // Floating SQUARE islands over a transparent surface — our take on
            // the islands look (ashell's are rounded; ours stay square/flat).
            let t = &self.config.theme;
            let base = t.background.base().0;
            let pillbg = Color::from_rgba(base[0], base[1], base[2], t.opacity);
            let r = t.radius.group();
            let bw = t.border.width.max(1.0);
            let bc = t.border.color.iced();
            let pill_style = move |_: &iced::Theme| container::Style {
                background: Some(Background::Color(pillbg)),
                border: Border {
                    color: bc,
                    width: bw,
                    radius: r.into(),
                },
                ..Default::default()
            };
            let ws_pill = container(ws_row)
                .padding([2, 10])
                .center_y(Length::Fill)
                .style(pill_style);
            let title_pill = container(title_el)
                .padding([2, 12])
                .center_y(Length::Fill)
                .style(pill_style);
            let right_pill = container(right_inner)
                .padding([2, 10])
                .center_y(Length::Fill)
                .style(pill_style);
            container(
                row![
                    ws_pill,
                    Space::new().width(Length::Fill),
                    title_pill,
                    Space::new().width(Length::Fill),
                    right_pill,
                ]
                .align_y(Vertical::Center),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .padding([4, 10])
            .into()
        } else {
            let left = container(ws_row)
                .width(Length::FillPortion(1))
                .align_x(Horizontal::Left)
                .center_y(Length::Fill)
                .padding([0, 8]);
            let center = container(title_el)
                .width(Length::FillPortion(1))
                .align_x(Horizontal::Center)
                .center_y(Length::Fill);
            let right_row = container(right_inner)
                .width(Length::FillPortion(1))
                .align_x(Horizontal::Right)
                .center_y(Length::Fill);
            container(row![left, center, right_row].align_y(Vertical::Center))
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
        let geom = if geom_changed {
            self.reconcile_bar_geometry(self.config.bar.position)
        } else {
            Task::none()
        };
        Task::batch([modules, geom])
    }

    /// RFC 0004: re-anchor / re-size / re-layer the *live* bar surface to match the
    /// current config (at position `pos`) by emitting iced_layershell's in-place
    /// mutation messages — no surface re-roll, no exit-on-close dance. The change
    /// messages are consumed by the layershell runtime (not our `update`). Updates
    /// `bar_pos` so a later diff is a no-op.
    fn reconcile_bar_geometry(&mut self, pos: config::Position) -> Task<Message> {
        let g = bar_geom(&self.config.bar, pos);
        let id = self.bar_id;
        self.bar_pos = pos;
        Task::batch([
            Task::done(Message::AnchorChange {
                id,
                anchor: g.anchor,
            }),
            Task::done(Message::MarginChange {
                id,
                margin: g.margin,
            }),
            Task::done(Message::ExclusiveZoneChange {
                id,
                zone_size: g.exclusive_zone,
            }),
            Task::done(Message::SizeChange { id, size: g.size }),
            Task::done(Message::LayerChange { id, layer: g.layer }),
        ])
    }

    /// Shut down the old modules and construct the new set from the current config.
    /// RFC 0004: reconcile the live module set against config, keyed by `key`.
    /// **Unchanged** instances keep their state, recipes, and running streams;
    /// **added** are constructed; **removed** are shut down; **config-changed** are
    /// `reconfigure`d (or rebuilt) with a generation bump that re-keys their
    /// subscriptions. Order follows placement. Returns a task to close a module
    /// popup whose owning instance went away.
    fn reconcile_modules(&mut self) -> Task<Message> {
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
                        if let Some(m) = modules::build(&s.type_id, id, &s.cfg) {
                            self.bump_generation(id);
                            next.push(ModuleEntry::new(id, s.key, m, s.cfg));
                        }
                    }
                },
                // added.
                None => {
                    if let Some(m) = modules::build(&s.type_id, id, &s.cfg) {
                        next.push(ModuleEntry::new(id, s.key, m, s.cfg));
                    }
                }
            }
        }
        // whatever is left was removed from placement.
        for (_, mut e) in live.drain() {
            e.module.shutdown();
            self.generation.remove(&e.id);
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
    config::load_result()
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
