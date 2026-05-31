use std::time::Duration;

use iced::alignment::{Horizontal, Vertical};
use iced::futures::{SinkExt, Stream};
use iced::widget::{canvas, column, container, mouse_area, row, scrollable, text, Space};
use iced::{event, window, Background, Border, Color, Element, Length, Subscription, Task};
use iced_layershell::build_pattern::daemon;
use iced_layershell::reexport::{
    Anchor, KeyboardInteractivity, Layer, NewLayerShellSettings, OutputOption,
};
use iced_layershell::settings::{LayerShellSettings, Settings, StartMode};
use iced_layershell::to_layer_message;

use ezbar::config::{self, Config, Style, SwitcherPos};
use ezbar::history::History;
use ezbar::modules;
use ezbar::sources::sway::{SwayUpdate, Workspace};
use ezbar::sources::{battery, calendar, kubectl, ping, spotify, stock, sway, system, volume};
use ezbar::widgets::graph::{Graph, GraphKind, StockChart};
use ezbar_plugin::{Ctx, HostRequest, ModMsg, Module, PopupMode, ThemeTokens};

mod install;

const PING_TARGET: &str = "8.8.8.8";
const BAR_HEIGHT: u32 = 34;

struct ModuleEntry {
    id: u64,
    name: String,
    module: Box<dyn Module>,
    disabled: bool,
}

impl ModuleEntry {
    fn new(id: u64, module: Box<dyn Module>) -> Self {
        let name = module.id().to_string();
        ModuleEntry {
            id,
            name,
            module,
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
    let font = match config::load().bar.font {
        Some(name) => iced::Font::with_name(Box::leak(name.into_boxed_str())),
        None => iced::Font::with_name("JetBrainsMono Nerd Font"),
    };
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
    Calendar,
    Kubectl,
    Stock,
    Switcher,
}

struct Bar {
    bar_id: window::Id,
    popup: Option<(window::Id, PopupKind)>,
    module_popup: Option<(window::Id, u64, PopupMode)>,
    modules: Vec<ModuleEntry>,

    // system
    mem_str: String,
    temp_str: String,
    ping: ping::PingData,
    time_str: String,
    battery_str: String,
    has_battery: bool,
    volume: volume::VolumeData,
    kubectl: kubectl::KubectlData,
    kubectl_contexts: Vec<String>,

    // graph histories
    mem_hist: History,
    temp_hist: History,
    ping_hist: History,

    // graph visibility (memory + ping start hidden, like the Go version)
    show_mem_graph: bool,
    show_temp_graph: bool,
    show_ping_graph: bool,

    // sway
    workspaces: Vec<Workspace>,
    title: String,

    // network widgets
    calendar: calendar::CalendarData,
    stock: stock::StockData,
    stock_symbol: String,
    stock_chart: Vec<f64>,
    spotify: spotify::SpotifyData,

    // animation / polish state
    blink_on: bool,
    spotify_offset: usize,

    // popup anchoring: last cursor x over the bar, so a popup opens above the
    // widget the user interacted with (RFC 0001 slot-derived).
    cursor_x: f32,

    // config + resolved module theme (RFC 0002)
    config: Config,
    theme: ThemeTokens,

    // temporary A/B switch for the workspace-chip style (env EZBAR_WS_STYLE)
    ws_style: u8,
}

#[to_layer_message(multi)]
#[derive(Debug, Clone)]
enum Message {
    Mem(String),
    Temp(String),
    Ping(ping::PingData),
    Time(String),
    Battery(String),
    Volume(volume::VolumeData),
    Kubectl(kubectl::KubectlData),
    KubectlContexts(Vec<String>),
    Sway(SwayUpdate),
    Calendar(calendar::CalendarData),
    Stock(stock::StockData),
    StockChart(Vec<f64>),
    Spotify(spotify::SpotifyData),

    ToggleGraph(GraphKind),
    VolumeClick,
    VolumeScroll(i32),
    KubectlClear,
    KubectlSelect(String),
    SpotifyClick,
    SpotifyScroll(bool),
    SwitchWorkspace(String),
    SelectPreset(String),
    OpenPopup(PopupKind),
    ClosePopup,
    BlinkTick,
    ConfigReloaded(Result<Config, String>),
    WindowClosed(window::Id),
    Cursor(window::Id, f32),
    Noop,
    ModuleMsg { instance: u64, msg: ModMsg },
}

fn bar_settings() -> NewLayerShellSettings {
    NewLayerShellSettings {
        size: Some((0, BAR_HEIGHT)),
        exclusive_zone: Some(BAR_HEIGHT as i32),
        anchor: Anchor::Bottom | Anchor::Left | Anchor::Right,
        layer: Layer::Top,
        keyboard_interactivity: KeyboardInteractivity::None,
        output_option: OutputOption::None,
        namespace: Some("ezbar".to_string()),
        ..Default::default()
    }
}

fn popup_size(kind: PopupKind) -> (u32, u32) {
    match kind {
        PopupKind::Calendar => (380, 320),
        PopupKind::Kubectl => (320, 360),
        PopupKind::Stock => (520, 300),
        PopupKind::Switcher => (220, 280),
    }
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

fn load_kubectl_contexts() -> Task<Message> {
    Task::perform(
        async {
            tokio::task::spawn_blocking(kubectl::get_all_contexts)
                .await
                .unwrap_or_default()
        },
        Message::KubectlContexts,
    )
}

impl Bar {
    fn new() -> (Self, Task<Message>) {
        let bar_id = window::Id::unique();
        let open = Task::done(Message::NewLayerShell {
            settings: bar_settings(),
            id: bar_id,
        });
        let config = config::load();
        let theme = config.theme_tokens();
        // Workspace chip style: config drives it; EZBAR_WS_STYLE overrides (dev A/B).
        let ws_style = std::env::var("EZBAR_WS_STYLE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| config.theme.workspaces.style.variant());
        let bar = Bar {
            bar_id,
            popup: None,
            module_popup: None,
            modules: vec![
                ModuleEntry::new(1, Box::new(modules::cpu::Cpu::new(1))),
                ModuleEntry::new(2, Box::new(modules::github::GitHub::new(2))),
                ModuleEntry::new(3, Box::new(modules::claude::Claude::new(3))),
            ],
            mem_str: " --".to_string(),
            temp_str: " --".to_string(),
            ping: ping::PingData::default(),
            time_str: "Loading…".to_string(),
            battery_str: " --".to_string(),
            has_battery: battery::has_battery(),
            volume: volume::VolumeData::default(),
            kubectl: kubectl::KubectlData::default(),
            kubectl_contexts: Vec::new(),
            mem_hist: History::new(20),
            temp_hist: History::new(60),
            ping_hist: History::new(40),
            show_mem_graph: false,
            show_temp_graph: true,
            show_ping_graph: false,
            workspaces: Vec::new(),
            title: String::new(),
            calendar: calendar::CalendarData {
                display_text: " …".to_string(),
                ..Default::default()
            },
            stock: stock::StockData {
                display_text: " …".to_string(),
                ..Default::default()
            },
            stock_symbol: stock::config().0,
            stock_chart: Vec::new(),
            spotify: spotify::SpotifyData::default(),
            blink_on: true,
            spotify_offset: 0,
            cursor_x: 0.0,
            config,
            theme,
            ws_style,
        };
        // Dev/screenshot hook: open a popup on startup for deterministic capture.
        if let Ok(k) = std::env::var("EZBAR_OPEN_POPUP") {
            let kind = match k.as_str() {
                "kubectl" => PopupKind::Kubectl,
                "stock" => PopupKind::Stock,
                "switcher" => PopupKind::Switcher,
                _ => PopupKind::Calendar,
            };
            return (
                bar,
                Task::batch([open, Task::done(Message::OpenPopup(kind))]),
            );
        }
        (bar, open)
    }

    fn namespace() -> String {
        "ezbar".to_string()
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Mem(s) => {
                self.mem_hist.add(system::extract_memory_usage_value(&s));
                self.mem_str = s;
                Task::none()
            }
            Message::Temp(s) => {
                self.temp_hist.add(system::extract_temperature_value(&s));
                self.temp_str = s;
                Task::none()
            }
            Message::Ping(d) => {
                if d.is_up {
                    self.ping_hist.add(d.latency);
                }
                self.ping = d;
                Task::none()
            }
            Message::Time(s) => {
                self.time_str = s;
                self.spotify_offset = self.spotify_offset.wrapping_add(1);
                Task::none()
            }
            Message::Battery(s) => {
                self.battery_str = s;
                Task::none()
            }
            Message::Volume(d) => {
                self.volume = d;
                Task::none()
            }
            Message::Kubectl(d) => {
                self.kubectl = d;
                Task::none()
            }
            Message::KubectlContexts(v) => {
                self.kubectl_contexts = v;
                Task::none()
            }
            Message::Sway(SwayUpdate::Workspaces(ws)) => {
                self.workspaces = ws;
                Task::none()
            }
            Message::Sway(SwayUpdate::Title(t)) => {
                self.title = t;
                Task::none()
            }
            Message::Calendar(d) => {
                self.calendar = d;
                Task::none()
            }
            Message::Stock(d) => {
                self.stock = d;
                Task::none()
            }
            Message::StockChart(v) => {
                self.stock_chart = v;
                Task::none()
            }
            Message::Spotify(d) => {
                self.spotify = d;
                Task::none()
            }
            Message::ToggleGraph(kind) => {
                match kind {
                    // cpu is a module now; it toggles its own graph internally.
                    GraphKind::Cpu => {}
                    GraphKind::Memory => self.show_mem_graph = !self.show_mem_graph,
                    GraphKind::Temperature => self.show_temp_graph = !self.show_temp_graph,
                    GraphKind::Ping => self.show_ping_graph = !self.show_ping_graph,
                }
                Task::none()
            }
            Message::VolumeClick => Task::perform(
                async {
                    tokio::task::spawn_blocking(|| {
                        volume::toggle_mute();
                        volume::update_volume()
                    })
                    .await
                    .unwrap_or_default()
                },
                Message::Volume,
            ),
            Message::VolumeScroll(dir) => Task::perform(
                async move {
                    tokio::task::spawn_blocking(move || {
                        volume::change_volume(dir);
                        volume::update_volume()
                    })
                    .await
                    .unwrap_or_default()
                },
                Message::Volume,
            ),
            Message::KubectlClear => Task::perform(
                async {
                    tokio::task::spawn_blocking(|| {
                        kubectl::clear_context();
                        kubectl::update_context()
                    })
                    .await
                    .unwrap_or_default()
                },
                Message::Kubectl,
            ),
            Message::KubectlSelect(ctx) => {
                let close = self.close_popup_task();
                let set = Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || {
                            kubectl::set_context(&ctx);
                            kubectl::update_context()
                        })
                        .await
                        .unwrap_or_default()
                    },
                    Message::Kubectl,
                );
                Task::batch([close, set])
            }
            Message::SpotifyClick => {
                let needs_auth = self.spotify.needs_auth;
                let is_playing = self.spotify.is_playing;
                Task::perform(
                    async move {
                        if needs_auth {
                            let _ = tokio::task::spawn_blocking(spotify::authorize).await;
                        } else {
                            spotify::toggle_playback(is_playing).await;
                        }
                        spotify::poll().await
                    },
                    Message::Spotify,
                )
            }
            Message::SpotifyScroll(next) => Task::perform(
                async move {
                    spotify::skip(next).await;
                    spotify::poll().await
                },
                Message::Spotify,
            ),
            Message::SwitchWorkspace(name) => Task::perform(
                async move {
                    let _ = tokio::task::spawn_blocking(move || {
                        if let Ok(mut conn) = swayipc::Connection::new() {
                            let _ = conn.run_command(format!("workspace {name}"));
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
                self.config = config::load();
                self.theme = self.config.theme_tokens();
                if std::env::var("EZBAR_WS_STYLE").is_err() {
                    self.ws_style = self.config.theme.workspaces.style.variant();
                }
                close
            }
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
                    return Task::batch([close_mod, close, open, self.popup_extra(kind)]);
                }
                let (id, open) = self.open_popup(kind);
                self.popup = Some((id, kind));
                Task::batch([close_mod, open, self.popup_extra(kind)])
            }
            Message::ClosePopup => self.close_popup_task(),
            Message::BlinkTick => {
                self.blink_on = !self.blink_on;
                Task::none()
            }
            Message::ConfigReloaded(Ok(cfg)) => {
                log::info!("config reloaded");
                self.config = cfg;
                self.theme = self.config.theme_tokens();
                // EZBAR_WS_STYLE (dev) still wins; otherwise follow the reloaded config.
                if std::env::var("EZBAR_WS_STYLE").is_err() {
                    self.ws_style = self.config.theme.workspaces.style.variant();
                }
                Task::none()
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

    /// Left margin so a `popup_w`-wide popup is centered above the cursor (i.e.
    /// the widget that triggered it), clamped to stay on the output.
    fn popup_left_margin(&self, popup_w: u32) -> i32 {
        // Center the popup above the cursor (i.e. the widget that triggered it),
        // clamped only to the left edge. No right clamp: no popup-bearing widget
        // sits within half a popup width of the bar's right edge.
        (self.cursor_x as i32 - popup_w as i32 / 2).max(0)
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

    /// One workspace rendered as a square, state-filled chip (our square/dark
    /// identity — not ashell's rounded morphing pill). State drives the *fill*,
    /// not the width; `cell_w` is a shared, uniform cell width so `1` and `10`
    /// read as the same square cell. `variant` (env `EZBAR_WS_STYLE`, 1-4) is a
    /// temporary A/B switch for design selection; the winner becomes the default.
    fn ws_chip<'a>(&'a self, w: &'a Workspace, variant: u8, cell_w: f32) -> Element<'a, Message> {
        let th = &self.config.theme;
        let accent = th.primary.iced();
        let fg = th.text.iced();
        let dim = th.dim.iced();
        let urg = th.urgent.iced();
        let base = th.background.base().iced(); // dark text on a bright fill
        let radius: f32 = 0.0; // square identity (theme.radius.item once wired)
        let fs = th.font_size;
        let chip_h = (self.config.bar.height as f32 - 10.0).max(14.0);

        let focused = w.focused;
        let visible = w.visible && !w.focused;
        let urgent = w.urgent;
        let blink = urgent && !self.blink_on;
        let fade = |c: Color| if blink { Color { a: 0.4, ..c } } else { c };
        let tint = |c: Color, a: f32| Color { a, ..c };

        // 4 — accent underbar: numbers + a 2px bar under the active ws.
        if variant == 4 {
            let (txt, bar_color) = if urgent {
                (fade(urg), fade(urg))
            } else if focused {
                (fg, accent)
            } else if visible {
                (fg, Color::TRANSPARENT)
            } else {
                (dim, Color::TRANSPARENT)
            };
            let label = container(text(w.name.clone()).size(fs).color(txt))
                .width(Length::Fixed(cell_w))
                .height(Length::Fill)
                .align_x(Horizontal::Center)
                .align_y(Vertical::Center);
            let underbar = container(Space::new().height(Length::Fixed(2.0)))
                .width(Length::Fixed(cell_w))
                .style(move |_: &iced::Theme| container::Style {
                    background: Some(Background::Color(bar_color)),
                    ..Default::default()
                });
            return mouse_area(column![label, underbar].height(Length::Fixed(chip_h)))
                .on_press(Message::SwitchWorkspace(w.name.clone()))
                .into();
        }

        // (background, border_width, border_color, text_color) per state.
        let (bg, bw, bc, txt): (Option<Color>, f32, Color, Color) = match variant {
            // 1 — filled focus only: just the active ws is a solid square.
            1 => {
                if urgent {
                    (Some(fade(urg)), 0.0, urg, base)
                } else if focused {
                    (Some(accent), 0.0, accent, base)
                } else if visible {
                    (None, 0.0, fg, fg)
                } else {
                    (None, 0.0, dim, dim)
                }
            }
            // 3 — outlined focus: square accent border, transparent fill.
            3 => {
                if urgent {
                    (None, 1.5, fade(urg), fade(urg))
                } else if focused {
                    (None, 1.5, accent, accent)
                } else if visible {
                    (None, 1.0, tint(fg, 0.35), fg)
                } else {
                    (None, 1.0, tint(fg, 0.12), dim)
                }
            }
            // 2 — all boxed (default): every ws a defined cell, tiered by state.
            // Each idle cell carries a hairline border so it separates cleanly
            // from the panel even at low fill (the v1 boxes were too muddy).
            _ => {
                if urgent {
                    (Some(fade(urg)), 0.0, urg, base)
                } else if focused {
                    (Some(accent), 0.0, accent, base)
                } else if visible {
                    (Some(tint(accent, 0.28)), 1.0, tint(accent, 0.55), fg)
                } else {
                    (Some(tint(fg, 0.10)), 1.0, tint(fg, 0.16), tint(fg, 0.78))
                }
            }
        };

        let inner = container(text(w.name.clone()).size(fs).color(txt))
            .width(Length::Fixed(cell_w))
            .height(Length::Fixed(chip_h))
            .align_x(Horizontal::Center)
            .align_y(Vertical::Center)
            .style(move |_: &iced::Theme| container::Style {
                background: bg.map(Background::Color),
                border: Border {
                    color: bc,
                    width: bw,
                    radius: radius.into(),
                },
                text_color: Some(txt),
                ..Default::default()
            });
        mouse_area(inner)
            .on_press(Message::SwitchWorkspace(w.name.clone()))
            .into()
    }

    fn close_popup_task(&mut self) -> Task<Message> {
        if let Some((pid, _)) = self.popup.take() {
            iced::window::close(pid)
        } else {
            Task::none()
        }
    }

    /// Side-effect task to run when a popup opens (load contexts / fetch chart).
    fn popup_extra(&self, kind: PopupKind) -> Task<Message> {
        match kind {
            PopupKind::Kubectl => load_kubectl_contexts(),
            PopupKind::Stock => {
                let sym = self.stock_symbol.clone();
                Task::perform(
                    async move { stock::fetch_chart(&sym).await },
                    Message::StockChart,
                )
            }
            _ => Task::none(),
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

    fn bar_view(&self) -> Element<'_, Message> {
        // ---- left: workspaces (square state-chips) ----
        // One shared, uniform cell width so single- and multi-digit names read as
        // the same square cell (fixes the "10 is a wide rectangle" wobble).
        let variant = self.ws_style;
        let fs = self.config.theme.font_size;
        let max_chars = self
            .workspaces
            .iter()
            .map(|w| w.name.chars().count())
            .max()
            .unwrap_or(1)
            .max(1) as f32;
        let chip_h = (self.config.bar.height as f32 - 10.0).max(14.0);
        let cell_w = (max_chars * fs * 0.62 + 8.0).max(chip_h);
        let mut ws_items: Vec<Element<Message>> = Vec::new();
        if self.config.bar.switcher == SwitcherPos::Left {
            let dim = self.config.theme.dim.iced();
            ws_items.push(
                mouse_area(
                    container(text("\u{f107}").size(fs).color(dim))
                        .padding([0, 4])
                        .center_y(Length::Fill),
                )
                .on_press(Message::OpenPopup(PopupKind::Switcher))
                .into(),
            );
        }
        ws_items.extend(
            self.workspaces
                .iter()
                .map(|w| self.ws_chip(w, variant, cell_w)),
        );
        let ws_row: Element<Message> = row(ws_items).spacing(4).align_y(Vertical::Center).into();

        // ---- center: focused window title ----
        let title_el: Element<Message> = text(self.title.clone()).into();

        // ---- right: widgets ----
        let mut right: Vec<Element<Message>> = Vec::new();
        let sep = || text("|").color(Color::from_rgb(0.4, 0.4, 0.4));

        // pluggable modules (RFC 0001) render first in the right zone.
        for entry in &self.modules {
            let instance = entry.id;
            if entry.disabled {
                // module panicked in update and was torn down — static error chip.
                right.push(
                    text(format!(" {}", entry.name))
                        .color(Color::from_rgb(1.0, 0.3, 0.3))
                        .into(),
                );
                right.push(sep().into());
                continue;
            }
            let ctx = Ctx {
                instance_id: instance,
                theme: &self.theme,
            };
            right.push(
                entry
                    .module
                    .view(&ctx)
                    .map(move |m| Message::ModuleMsg { instance, msg: m }),
            );
            right.push(sep().into());
        }

        // calendar:  <text> [<countdown>], coloured by urgency; click for popup
        {
            let c = &self.calendar;
            let base = if c.is_overdue {
                Color::from_rgb(1.0, 0.27, 0.27)
            } else if c.is_urgent {
                Color::from_rgb(1.0, 0.67, 0.0)
            } else {
                Color::WHITE
            };
            let blinking = c.is_overdue || c.is_urgent;
            let color = if blinking && !self.blink_on {
                Color { a: 0.4, ..base }
            } else {
                base
            };
            let mut cal: Vec<Element<Message>> = vec![
                text("").into(),
                text(c.display_text.clone()).color(color).into(),
            ];
            if !c.time_until_next.is_empty() {
                cal.push(text(format!("[{}]", c.time_until_next)).color(color).into());
            }
            right.push(
                mouse_area(row(cal).spacing(4).align_y(Vertical::Center))
                    .on_press(Message::OpenPopup(PopupKind::Calendar))
                    .into(),
            );
        }
        right.push(sep().into());

        // kubectl (left-click clears; right-click opens context menu; red when production)
        let kube_color = if self.kubectl.is_production {
            Color::from_rgb(1.0, 0.2, 0.2)
        } else {
            Color::WHITE
        };
        right.push(
            mouse_area(text(self.kubectl.string.clone()).color(kube_color))
                .on_press(Message::KubectlClear)
                .on_right_press(Message::OpenPopup(PopupKind::Kubectl))
                .into(),
        );
        right.push(sep().into());

        right.push(self.metric(
            &self.temp_str,
            self.show_temp_graph,
            &self.temp_hist,
            GraphKind::Temperature,
        ));
        right.push(sep().into());
        right.push(self.metric(
            &self.mem_str,
            self.show_mem_graph,
            &self.mem_hist,
            GraphKind::Memory,
        ));
        right.push(sep().into());
        right.push(self.metric(
            &self.ping.string,
            self.show_ping_graph,
            &self.ping_hist,
            GraphKind::Ping,
        ));
        right.push(sep().into());

        // spotify: marquee long titles; click to play/pause/authorize, scroll to skip tracks
        right.push(
            mouse_area(text(marquee(
                &self.spotify.track_string,
                self.spotify_offset,
                40,
            )))
            .on_press(Message::SpotifyClick)
            .on_scroll(|delta| {
                let y = match delta {
                    iced::mouse::ScrollDelta::Lines { y, .. } => y,
                    iced::mouse::ScrollDelta::Pixels { y, .. } => y,
                };
                Message::SpotifyScroll(y > 0.0)
            })
            .into(),
        );
        right.push(sep().into());

        // stock: green up / red down
        {
            let s = &self.stock;
            let color = if s.is_positive && s.change != 0.0 {
                Color::from_rgb(0.2, 0.8, 0.2)
            } else if s.is_negative {
                Color::from_rgb(1.0, 0.3, 0.3)
            } else {
                Color::WHITE
            };
            right.push(
                mouse_area(text(s.display_text.clone()).color(color))
                    .on_enter(Message::OpenPopup(PopupKind::Stock))
                    .on_exit(Message::ClosePopup)
                    .into(),
            );
        }
        right.push(sep().into());

        // volume (click mute, scroll change)
        right.push(
            mouse_area(text(self.volume.string.clone()))
                .on_press(Message::VolumeClick)
                .on_scroll(|delta| {
                    let y = match delta {
                        iced::mouse::ScrollDelta::Lines { y, .. } => y,
                        iced::mouse::ScrollDelta::Pixels { y, .. } => y,
                    };
                    Message::VolumeScroll(if y > 0.0 { 1 } else { -1 })
                })
                .into(),
        );
        right.push(sep().into());

        if self.has_battery {
            right.push(text(self.battery_str.clone()).into());
            right.push(sep().into());
        }

        right.push(text(self.time_str.clone()).into());

        // The ▾ preset switcher (RFC 0002), if enabled for the right side.
        let switcher_btn = || -> Element<Message> {
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
        };
        if self.config.bar.switcher == SwitcherPos::Right {
            right.push(switcher_btn());
        }

        let right_inner: Element<Message> = row(right).spacing(6).align_y(Vertical::Center).into();

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

    /// A metric label plus optional canvas graph; click toggles graph visibility.
    fn metric<'a>(
        &self,
        label: &str,
        show_graph: bool,
        hist: &History,
        kind: GraphKind,
    ) -> Element<'a, Message> {
        let lbl = mouse_area(text(label.to_string())).on_press(Message::ToggleGraph(kind));
        if show_graph {
            let g = canvas(Graph {
                values: hist.ordered(),
                kind,
            })
            .width(Length::Fixed(80.0))
            .height(Length::Fixed(20.0));
            row![lbl, g].spacing(4).align_y(Vertical::Center).into()
        } else {
            lbl.into()
        }
    }

    // ---- popups ----

    fn popup_view(&self, kind: PopupKind) -> Element<'_, Message> {
        let body: Element<Message> = match kind {
            PopupKind::Calendar => self.calendar_popup(),
            PopupKind::Kubectl => self.kubectl_popup(),
            PopupKind::Stock => self.stock_popup(),
            PopupKind::Switcher => self.switcher_popup(),
        };
        container(body)
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(12)
            .style(self.popup_style())
            .into()
    }

    fn calendar_popup(&self) -> Element<'_, Message> {
        let mut col: Vec<Element<Message>> = vec![text("Today's Meetings").size(15).into()];
        let now = chrono::Local::now();
        let mut any = false;
        // all-day first, then timed
        for ev in self.calendar.today_events.iter().filter(|e| e.is_all_day) {
            col.push(calendar_row("All day", &ev.title, Color::WHITE));
            any = true;
        }
        for ev in self.calendar.today_events.iter().filter(|e| !e.is_all_day) {
            let color = if now > ev.end {
                Color::from_rgb(0.4, 0.4, 0.4)
            } else if now > ev.start {
                Color::from_rgb(0.0, 1.0, 0.0)
            } else if (ev.start - now) <= chrono::Duration::minutes(15) {
                Color::from_rgb(1.0, 0.67, 0.0)
            } else {
                Color::WHITE
            };
            col.push(calendar_row(
                &ev.start.format("%H:%M").to_string(),
                &ev.title,
                color,
            ));
            any = true;
        }
        if !any {
            col.push(text("No meetings today").into());
        }
        scrollable(column(col).spacing(6)).into()
    }

    fn kubectl_popup(&self) -> Element<'_, Message> {
        let mut col: Vec<Element<Message>> = vec![text("Kubectl Context").size(15).into()];
        if self.kubectl_contexts.is_empty() {
            col.push(
                text("(no contexts)")
                    .color(Color::from_rgb(0.5, 0.5, 0.5))
                    .into(),
            );
        }
        for ctx in &self.kubectl_contexts {
            let is_current = *ctx == self.kubectl.context;
            let is_prod = kubectl::is_production_context(ctx);
            let color = if is_prod {
                Color::from_rgb(1.0, 0.4, 0.4)
            } else if is_current {
                Color::from_rgb(0.4, 0.9, 0.4)
            } else {
                Color::WHITE
            };
            let marker = if is_current { "▸ " } else { "  " };
            col.push(
                mouse_area(text(format!("{marker}{ctx}")).color(color))
                    .on_press(Message::KubectlSelect(ctx.clone()))
                    .into(),
            );
        }
        scrollable(column(col).spacing(4)).into()
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
            let (marker, color) = if is_current {
                ("\u{f00c} ", accent) // check
            } else {
                ("  ", fg)
            };
            col.push(
                mouse_area(text(format!("{marker}{name}")).color(color))
                    .on_press(Message::SelectPreset(name.clone()))
                    .into(),
            );
        }
        scrollable(column(col).spacing(6)).into()
    }

    fn stock_popup(&self) -> Element<'_, Message> {
        canvas(StockChart {
            values: self.stock_chart.clone(),
            symbol: self.stock_symbol.clone(),
        })
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
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
            Subscription::run(mem_stream),
            Subscription::run(config_stream),
            Subscription::run(temp_stream),
            Subscription::run(ping_stream),
            Subscription::run(time_stream),
            Subscription::run(volume_stream),
            Subscription::run(kubectl_stream),
            Subscription::run(calendar_stream),
            Subscription::run(stock_stream),
            Subscription::run(spotify_stream),
            Subscription::run(sway::sway_stream).map(Message::Sway),
            iced::time::every(Duration::from_millis(500)).map(|_| Message::BlinkTick),
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
            let instance = entry.id;
            subs.push(
                entry
                    .module
                    .subscription()
                    .with(instance)
                    .map(|(instance, m)| Message::ModuleMsg { instance, msg: m }),
            );
        }
        if self.has_battery {
            subs.push(Subscription::run(battery_stream));
        }
        Subscription::batch(subs)
    }
}

fn calendar_row<'a>(time: &str, title: &str, color: Color) -> Element<'a, Message> {
    row![
        text(time.to_string())
            .color(color)
            .width(Length::Fixed(60.0)),
        text(truncate(title, 40)).color(color).width(Length::Fill),
    ]
    .spacing(8)
    .into()
}

fn truncate(s: &str, max_len: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max_len {
        return s.to_string();
    }
    let mut out: String = chars[..max_len.saturating_sub(2)].iter().collect();
    out.push_str("..");
    out
}

/// Rotating marquee window over `s` (with trailing padding) when it exceeds `max_len`.
fn marquee(s: &str, offset: usize, max_len: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max_len {
        return s.to_string();
    }
    let padded: Vec<char> = s.chars().chain("    ".chars()).collect();
    let n = padded.len();
    let start = offset % n;
    (0..max_len).map(|i| padded[(start + i) % n]).collect()
}

// ---- subscription streams (one per data source) ----

fn read_parse(path: &std::path::Path) -> Result<Config, String> {
    match std::fs::read_to_string(path) {
        Ok(s) => config::parse_str(&s),
        Err(_) => Ok(Config::default()), // file gone → revert to defaults
    }
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

fn mem_stream() -> impl Stream<Item = Message> {
    iced::stream::channel(
        1,
        |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
            loop {
                let s = tokio::task::spawn_blocking(system::get_memory_usage)
                    .await
                    .unwrap_or_else(|_| " --".to_string());
                let _ = output.send(Message::Mem(s)).await;
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        },
    )
}

fn temp_stream() -> impl Stream<Item = Message> {
    iced::stream::channel(
        1,
        |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
            loop {
                let s = tokio::task::spawn_blocking(system::get_cpu_temperature)
                    .await
                    .unwrap_or_else(|_| " --".to_string());
                let _ = output.send(Message::Temp(s)).await;
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        },
    )
}

fn ping_stream() -> impl Stream<Item = Message> {
    iced::stream::channel(
        1,
        |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
            loop {
                let d = tokio::task::spawn_blocking(|| ping::perform_ping(PING_TARGET))
                    .await
                    .unwrap_or_default();
                let _ = output.send(Message::Ping(d)).await;
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        },
    )
}

fn time_stream() -> impl Stream<Item = Message> {
    iced::stream::channel(
        1,
        |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
            loop {
                let s = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
                let _ = output.send(Message::Time(s)).await;
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        },
    )
}

fn volume_stream() -> impl Stream<Item = Message> {
    iced::stream::channel(
        1,
        |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
            loop {
                let d = tokio::task::spawn_blocking(volume::update_volume)
                    .await
                    .unwrap_or_default();
                let _ = output.send(Message::Volume(d)).await;
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        },
    )
}

fn kubectl_stream() -> impl Stream<Item = Message> {
    iced::stream::channel(
        1,
        |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
            loop {
                let d = tokio::task::spawn_blocking(kubectl::update_context)
                    .await
                    .unwrap_or_default();
                let _ = output.send(Message::Kubectl(d)).await;
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        },
    )
}

fn battery_stream() -> impl Stream<Item = Message> {
    iced::stream::channel(
        1,
        |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
            loop {
                let s = tokio::task::spawn_blocking(battery::get_battery_status)
                    .await
                    .unwrap_or_else(|_| " --".to_string());
                let _ = output.send(Message::Battery(s)).await;
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        },
    )
}

fn calendar_stream() -> impl Stream<Item = Message> {
    iced::stream::channel(
        1,
        |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
            loop {
                match calendar::get_events().await {
                    Ok(d) => {
                        let _ = output.send(Message::Calendar(d)).await;
                    }
                    Err(e) => {
                        log::warn!("calendar: {e}");
                        let _ = output
                            .send(Message::Calendar(calendar::CalendarData {
                                display_text: "Setup: ~/.config/ezbar/calendar_url".to_string(),
                                ..Default::default()
                            }))
                            .await;
                    }
                }
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        },
    )
}

fn stock_stream() -> impl Stream<Item = Message> {
    iced::stream::channel(
        1,
        |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
            let (symbol, api_key) = stock::config();
            loop {
                match stock::fetch(&symbol, &api_key).await {
                    Ok(d) => {
                        let _ = output.send(Message::Stock(d)).await;
                    }
                    Err(e) => log::warn!("stock: {e}"),
                }
                tokio::time::sleep(Duration::from_secs(300)).await;
            }
        },
    )
}

fn spotify_stream() -> impl Stream<Item = Message> {
    iced::stream::channel(
        1,
        |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
            loop {
                let d = spotify::poll().await;
                let _ = output.send(Message::Spotify(d)).await;
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_ellipsizes() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("abcdefghij", 10), "abcdefghij");
        assert_eq!(truncate("abcdefghijk", 5), "abc..");
    }

    #[test]
    fn marquee_short_unchanged() {
        assert_eq!(marquee("hello", 0, 10), "hello");
        assert_eq!(marquee("hello", 99, 10), "hello");
    }

    #[test]
    fn marquee_long_rotates() {
        let s = "0123456789ABCDEF"; // 16 > 10
        assert_eq!(marquee(s, 0, 10), "0123456789");
        assert_eq!(marquee(s, 1, 10), "123456789A");
    }
}
