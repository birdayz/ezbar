use std::time::{Duration, Instant};

use iced::alignment::{Horizontal, Vertical};
use iced::futures::{SinkExt, Stream};
use iced::widget::{canvas, column, container, mouse_area, row, scrollable, text, Space};
use iced::{event, window, Background, Border, Color, Element, Length, Subscription, Task};
use iced_layershell::build_pattern::daemon;
use iced_layershell::reexport::{Anchor, KeyboardInteractivity, Layer, NewLayerShellSettings, OutputOption};
use iced_layershell::settings::{LayerShellSettings, Settings, StartMode};
use iced_layershell::to_layer_message;

mod history;
mod sources;
mod widgets;

use history::History;
use sources::sway::{SwayUpdate, Workspace};
use sources::{
    battery, calendar, claude, github, kubectl, ping, spotify, stock, sway, system, volume,
};
use widgets::graph::{Graph, GraphKind, StockChart};

const PING_TARGET: &str = "8.8.8.8";
const BAR_HEIGHT: u32 = 34;

fn main() -> iced_layershell::Result {
    env_logger::init();

    // Launcher: re-spawn the bar child forever (restart on crash / monitor change),
    // unless we ARE the child. A short backoff avoids hot-spinning (improves on the Go original).
    if std::env::var("EZBAR_CHILD").as_deref() != Ok("1") {
        loop {
            let exe = std::env::current_exe().expect("current_exe");
            match std::process::Command::new(exe).env("EZBAR_CHILD", "1").status() {
                Ok(s) if s.success() => log::info!("child exited cleanly"),
                Ok(s) => log::error!("child crashed: {:?}", s.code()),
                Err(e) => log::error!("failed to spawn child: {e}"),
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    run_bar()
}

fn run_bar() -> iced_layershell::Result {
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
        .run()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PopupKind {
    Calendar,
    GitHub,
    Kubectl,
    Stock,
    Claude,
}

struct Bar {
    bar_id: window::Id,
    popup: Option<(window::Id, PopupKind)>,

    // system
    cpu_str: String,
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
    cpu_hist: History,
    mem_hist: History,
    temp_hist: History,
    ping_hist: History,

    // graph visibility (memory + ping start hidden, like the Go version)
    show_cpu_graph: bool,
    show_mem_graph: bool,
    show_temp_graph: bool,
    show_ping_graph: bool,

    // sway
    workspaces: Vec<Workspace>,
    title: String,

    // network widgets
    calendar: calendar::CalendarData,
    github: github::GitHubData,
    github_token: Option<String>,
    stock: stock::StockData,
    stock_symbol: String,
    stock_chart: Vec<f64>,
    spotify: spotify::SpotifyData,

    // claude code status
    claude_instances: Vec<claude::Instance>,
    claude_block: Option<claude::Block>,
    claude_limits: Option<claude::Limits>,

    // animation / polish state
    blink_on: bool,
    spotify_offset: usize,
    github_prev_count: usize,
    github_blink_until: Option<Instant>,
}

#[to_layer_message(multi)]
#[derive(Debug, Clone)]
enum Message {
    Cpu(String),
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
    GitHub(github::GitHubData),
    Stock(stock::StockData),
    StockChart(Vec<f64>),
    Spotify(spotify::SpotifyData),
    ClaudeInstances(Vec<claude::Instance>),
    ClaudeBlock(Option<claude::Block>),
    ClaudeLimits(Option<claude::Limits>),

    ToggleGraph(GraphKind),
    VolumeClick,
    VolumeScroll(i32),
    KubectlClear,
    KubectlSelect(String),
    SpotifyClick,
    SpotifyScroll(bool),
    OpenPopup(PopupKind),
    ClosePopup,
    GithubMarkAll,
    GithubMarkRead(String),
    GithubOpen(String, String),
    BlinkTick,
    WindowClosed(window::Id),
    Noop,
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

fn popup_settings(kind: PopupKind) -> NewLayerShellSettings {
    let (w, h, right) = match kind {
        PopupKind::Calendar => (380u32, 320u32, 220i32),
        PopupKind::GitHub => (480, 440, 120),
        PopupKind::Kubectl => (320, 360, 40),
        PopupKind::Stock => (520, 300, 40),
        PopupKind::Claude => (460, 340, 40),
    };
    NewLayerShellSettings {
        size: Some((w, h)),
        exclusive_zone: None,
        anchor: Anchor::Bottom | Anchor::Right,
        layer: Layer::Overlay,
        // margin order: (top, right, bottom, left); float above the bar.
        margin: Some((0, right, BAR_HEIGHT as i32 + 6, 0)),
        keyboard_interactivity: KeyboardInteractivity::None,
        output_option: OutputOption::None,
        namespace: Some("ezbar-popup".to_string()),
        ..Default::default()
    }
}

fn open_popup(kind: PopupKind) -> (window::Id, Task<Message>) {
    let id = window::Id::unique();
    (
        id,
        Task::done(Message::NewLayerShell {
            settings: popup_settings(kind),
            id,
        }),
    )
}

fn load_kubectl_contexts() -> Task<Message> {
    Task::perform(
        async { tokio::task::spawn_blocking(kubectl::get_all_contexts).await.unwrap_or_default() },
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
        let bar = Bar {
            bar_id,
            popup: None,
            cpu_str: "🖥️ --".to_string(),
            mem_str: "💾 --".to_string(),
            temp_str: "🌡️ --".to_string(),
            ping: ping::PingData::default(),
            time_str: "Loading…".to_string(),
            battery_str: "🔋 --".to_string(),
            has_battery: battery::has_battery(),
            volume: volume::VolumeData::default(),
            kubectl: kubectl::KubectlData::default(),
            kubectl_contexts: Vec::new(),
            cpu_hist: History::new(30),
            mem_hist: History::new(20),
            temp_hist: History::new(60),
            ping_hist: History::new(40),
            show_cpu_graph: true,
            show_mem_graph: false,
            show_temp_graph: true,
            show_ping_graph: false,
            workspaces: Vec::new(),
            title: String::new(),
            calendar: calendar::CalendarData {
                display_text: "📅 …".to_string(),
                ..Default::default()
            },
            github: github::GitHubData {
                display_text: "GH …".to_string(),
                ..Default::default()
            },
            github_token: github::find_token(),
            stock: stock::StockData {
                display_text: "📈 …".to_string(),
                ..Default::default()
            },
            stock_symbol: stock::config().0,
            stock_chart: Vec::new(),
            spotify: spotify::SpotifyData::default(),
            claude_instances: Vec::new(),
            claude_block: None,
            claude_limits: None,
            blink_on: true,
            spotify_offset: 0,
            github_prev_count: 0,
            github_blink_until: None,
        };
        (bar, open)
    }

    fn namespace() -> String {
        "ezbar".to_string()
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Cpu(s) => {
                self.cpu_hist.add(system::extract_cpu_usage_value(&s));
                self.cpu_str = s;
                Task::none()
            }
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
            Message::GitHub(d) => {
                if d.count > self.github_prev_count {
                    self.github_blink_until = Some(Instant::now() + Duration::from_secs(30));
                }
                self.github_prev_count = d.count;
                self.github = d;
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
            Message::ClaudeInstances(v) => {
                self.claude_instances = v;
                Task::none()
            }
            Message::ClaudeBlock(b) => {
                self.claude_block = b;
                Task::none()
            }
            Message::ClaudeLimits(l) => {
                self.claude_limits = l;
                Task::none()
            }
            Message::ToggleGraph(kind) => {
                match kind {
                    GraphKind::Cpu => self.show_cpu_graph = !self.show_cpu_graph,
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
            Message::OpenPopup(kind) => {
                // Toggle off if the same popup is already open.
                if let Some((pid, k)) = self.popup {
                    if k == kind {
                        self.popup = None;
                        return iced::window::close(pid);
                    }
                    let close = iced::window::close(pid);
                    let (id, open) = open_popup(kind);
                    self.popup = Some((id, kind));
                    return Task::batch([close, open, self.popup_extra(kind)]);
                }
                let (id, open) = open_popup(kind);
                self.popup = Some((id, kind));
                Task::batch([open, self.popup_extra(kind)])
            }
            Message::ClosePopup => self.close_popup_task(),
            Message::GithubMarkAll => {
                let close = self.close_popup_task();
                self.github = github::GitHubData {
                    display_text: "GH 0".to_string(),
                    ..Default::default()
                };
                if let Some(token) = self.github_token.clone() {
                    let t = Task::perform(
                        async move { github::mark_all_as_read(&token).await },
                        |_| Message::Noop,
                    );
                    Task::batch([close, t])
                } else {
                    close
                }
            }
            Message::GithubMarkRead(nid) => {
                self.remove_github(&nid);
                if let Some(token) = self.github_token.clone() {
                    Task::perform(async move { github::mark_as_read(&token, &nid).await }, |_| {
                        Message::Noop
                    })
                } else {
                    Task::none()
                }
            }
            Message::GithubOpen(url, nid) => {
                let _ = std::process::Command::new("xdg-open").arg(&url).spawn();
                self.remove_github(&nid);
                if let Some(token) = self.github_token.clone() {
                    Task::perform(async move { github::mark_as_read(&token, &nid).await }, |_| {
                        Message::Noop
                    })
                } else {
                    Task::none()
                }
            }
            Message::BlinkTick => {
                self.blink_on = !self.blink_on;
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
                Task::none()
            }
            _ => Task::none(),
        }
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
                Task::perform(async move { stock::fetch_chart(&sym).await }, Message::StockChart)
            }
            _ => Task::none(),
        }
    }

    fn remove_github(&mut self, id: &str) {
        self.github.notifications.retain(|n| n.id != id);
        self.github.count = self.github.notifications.len();
        self.github.display_text = format!("GH {}", self.github.count);
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
        Space::new().into()
    }

    fn bar_view(&self) -> Element<'_, Message> {
        // ---- left: workspaces ----
        let mut ws_items: Vec<Element<Message>> = Vec::new();
        for w in &self.workspaces {
            let label = if w.focused {
                format!("[{}]", w.name)
            } else {
                format!(" {} ", w.name)
            };
            let color = if w.focused {
                Color::WHITE
            } else {
                Color::from_rgb(0.55, 0.55, 0.55)
            };
            ws_items.push(text(label).color(color).into());
        }
        let left = container(row(ws_items))
            .width(Length::FillPortion(1))
            .align_x(Horizontal::Left)
            .center_y(Length::Fill);

        // ---- center: focused window title ----
        let center = container(text(self.title.clone()))
            .width(Length::FillPortion(1))
            .align_x(Horizontal::Center)
            .center_y(Length::Fill);

        // ---- right: widgets ----
        let mut right: Vec<Element<Message>> = Vec::new();
        let sep = || text("|").color(Color::from_rgb(0.4, 0.4, 0.4));

        // claude code: instances + 5h limit % + block cost; hover for details
        {
            let n = self.claude_instances.len();
            let waiting = self.claude_instances.iter().filter(|i| i.waiting).count();
            let count_color = if waiting > 0 {
                Color::from_rgb(1.0, 0.8, 0.2)
            } else {
                Color::WHITE
            };
            let mut items: Vec<Element<Message>> =
                vec![text("🤖").into(), text(format!("{}", n)).color(count_color).into()];
            if let Some(p) = self.claude_limits.as_ref().and_then(|l| l.five_h_left) {
                let c = if p < 15.0 {
                    Color::from_rgb(1.0, 0.2, 0.2)
                } else if p < 30.0 {
                    Color::from_rgb(1.0, 0.67, 0.0)
                } else {
                    Color::from_rgb(0.6, 0.8, 1.0)
                };
                items.push(text(format!("5h{:.0}%", p)).color(c).into());
            }
            if let Some(b) = &self.claude_block {
                items.push(
                    text(format!("${:.0}", b.cost))
                        .color(Color::from_rgb(0.7, 0.7, 0.7))
                        .into(),
                );
            }
            right.push(
                mouse_area(row(items).spacing(4).align_y(Vertical::Center))
                    .on_enter(Message::OpenPopup(PopupKind::Claude))
                    .on_exit(Message::ClosePopup)
                    .into(),
            );
            right.push(sep().into());
        }

        // calendar: 📅 <text> [<countdown>], coloured by urgency; click for popup
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
            let mut cal: Vec<Element<Message>> =
                vec![text("📅").into(), text(c.display_text.clone()).color(color).into()];
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

        // github: GH N — blue when there are notifications, blinks red on new; click for popup
        {
            let blinking = self
                .github_blink_until
                .map(|t| Instant::now() < t)
                .unwrap_or(false);
            let base = if blinking {
                Color::from_rgb(1.0, 0.27, 0.27)
            } else if self.github.count > 0 {
                Color::from_rgb(0.345, 0.65, 1.0)
            } else {
                Color::WHITE
            };
            let color = if blinking && !self.blink_on {
                Color { a: 0.35, ..base }
            } else {
                base
            };
            right.push(
                mouse_area(text(self.github.display_text.clone()).color(color))
                    .on_press(Message::OpenPopup(PopupKind::GitHub))
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

        right.push(self.metric(&self.cpu_str, self.show_cpu_graph, &self.cpu_hist, GraphKind::Cpu));
        right.push(sep().into());
        right.push(self.metric(&self.temp_str, self.show_temp_graph, &self.temp_hist, GraphKind::Temperature));
        right.push(sep().into());
        right.push(self.metric(&self.mem_str, self.show_mem_graph, &self.mem_hist, GraphKind::Memory));
        right.push(sep().into());
        right.push(self.metric(&self.ping.string, self.show_ping_graph, &self.ping_hist, GraphKind::Ping));
        right.push(sep().into());

        // spotify: marquee long titles; click to play/pause/authorize, scroll to skip tracks
        right.push(
            mouse_area(text(marquee(&self.spotify.track_string, self.spotify_offset, 40)))
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

        let right_row = container(row(right).spacing(6).align_y(Vertical::Center))
            .width(Length::FillPortion(1))
            .align_x(Horizontal::Right)
            .center_y(Length::Fill);

        container(row![left, center, right_row].align_y(Vertical::Center))
            .width(Length::Fill)
            .height(Length::Fill)
            .padding([0, 8])
            .into()
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
            PopupKind::GitHub => self.github_popup(),
            PopupKind::Kubectl => self.kubectl_popup(),
            PopupKind::Stock => self.stock_popup(),
            PopupKind::Claude => self.claude_popup(),
        };
        container(body)
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(12)
            .style(|_theme| container::Style {
                background: Some(Background::Color(Color::from_rgba(0.0, 0.0, 0.0, 0.92))),
                border: Border {
                    color: Color::from_rgb(0.2, 0.2, 0.2),
                    width: 1.0,
                    radius: 8.0.into(),
                },
                text_color: Some(Color::WHITE),
                ..Default::default()
            })
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
            col.push(calendar_row(&ev.start.format("%H:%M").to_string(), &ev.title, color));
            any = true;
        }
        if !any {
            col.push(text("No meetings today").into());
        }
        scrollable(column(col).spacing(6)).into()
    }

    fn github_popup(&self) -> Element<'_, Message> {
        let mut header: Vec<Element<Message>> = vec![text(format!(
            "GitHub Notifications ({})",
            self.github.count
        ))
        .size(15)
        .width(Length::Fill)
        .into()];
        header.push(
            mouse_area(text("[clear all]").color(Color::from_rgb(0.55, 0.65, 0.8)))
                .on_press(Message::GithubMarkAll)
                .into(),
        );

        let mut col: Vec<Element<Message>> =
            vec![row(header).spacing(8).align_y(Vertical::Center).into()];

        let reason_order = [
            "review_requested",
            "mention",
            "assign",
            "author",
            "comment",
            "state_change",
            "manual",
            "subscribed",
        ];
        for reason in reason_order {
            let group: Vec<&github::GitHubNotification> = self
                .github
                .notifications
                .iter()
                .filter(|n| n.reason == reason)
                .collect();
            if group.is_empty() {
                continue;
            }
            col.push(
                text(format!(
                    "{} ({})",
                    github::reason_display_name(reason),
                    group.len()
                ))
                .color(Color::from_rgb(0.345, 0.65, 1.0))
                .into(),
            );
            for n in group.iter().take(10) {
                col.push(github_row(n));
            }
            if group.len() > 10 {
                col.push(
                    text(format!("  … and {} more", group.len() - 10))
                        .color(Color::from_rgb(0.4, 0.4, 0.4))
                        .into(),
                );
            }
        }
        if self.github.notifications.is_empty() {
            col.push(text("No notifications").into());
        }
        scrollable(column(col).spacing(4)).into()
    }

    fn kubectl_popup(&self) -> Element<'_, Message> {
        let mut col: Vec<Element<Message>> = vec![text("Kubectl Context").size(15).into()];
        if self.kubectl_contexts.is_empty() {
            col.push(text("(no contexts)").color(Color::from_rgb(0.5, 0.5, 0.5)).into());
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

    fn stock_popup(&self) -> Element<'_, Message> {
        canvas(StockChart {
            values: self.stock_chart.clone(),
            symbol: self.stock_symbol.clone(),
        })
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }

    fn claude_popup(&self) -> Element<'_, Message> {
        let header = Color::from_rgb(0.345, 0.65, 1.0);
        let dim = Color::from_rgb(0.7, 0.7, 0.7);
        let mut col: Vec<Element<Message>> = vec![text(format!(
            "Claude — {} instance(s)",
            self.claude_instances.len()
        ))
        .size(15)
        .into()];

        for i in &self.claude_instances {
            let (marker, color) = if i.waiting {
                ("⏳", Color::from_rgb(1.0, 0.8, 0.2))
            } else {
                ("▶", Color::from_rgb(0.5, 0.85, 0.5))
            };
            col.push(
                row![text(marker), text(i.project.clone()).color(color)]
                    .spacing(8)
                    .into(),
            );
        }

        if let Some(b) = &self.claude_block {
            col.push(text("5-hour block").color(header).into());
            col.push(
                text(format!(
                    "  ${:.2} · ${:.0}/hr · {}m left · resets {}",
                    b.cost, b.burn_per_hour, b.minutes_left, b.reset
                ))
                .into(),
            );
            col.push(
                text(format!("  projected ${:.0} · {}", b.projected_cost, b.model))
                    .color(dim)
                    .into(),
            );
        }

        if let Some(l) = &self.claude_limits {
            col.push(text("Limits").color(header).into());
            if let Some(p) = l.five_h_left {
                col.push(text(format!("  5h: {:.0}% left · resets {}", p, l.five_h_reset)).into());
            }
            if let Some(p) = l.weekly_left {
                col.push(
                    text(format!("  weekly: {:.0}% left · resets {}", p, l.weekly_reset)).into(),
                );
            }
        }

        scrollable(column(col).spacing(4)).into()
    }

    fn style(&self, _theme: &iced::Theme) -> iced::theme::Style {
        iced::theme::Style {
            background_color: Color::from_rgba(0.0, 0.0, 0.0, 0.8),
            text_color: Color::WHITE,
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        let mut subs = vec![
            Subscription::run(cpu_stream),
            Subscription::run(mem_stream),
            Subscription::run(temp_stream),
            Subscription::run(ping_stream),
            Subscription::run(time_stream),
            Subscription::run(volume_stream),
            Subscription::run(kubectl_stream),
            Subscription::run(calendar_stream),
            Subscription::run(github_stream),
            Subscription::run(stock_stream),
            Subscription::run(spotify_stream),
            Subscription::run(claude_instances_stream),
            Subscription::run(claude_block_stream),
            Subscription::run(claude_limits_stream),
            Subscription::run(sway::sway_stream).map(Message::Sway),
            iced::time::every(Duration::from_millis(500)).map(|_| Message::BlinkTick),
            event::listen_with(|ev, _status, id| match ev {
                iced::Event::Window(iced::window::Event::Closed) => Some(Message::WindowClosed(id)),
                _ => None,
            }),
        ];
        if self.has_battery {
            subs.push(Subscription::run(battery_stream));
        }
        Subscription::batch(subs)
    }
}

fn calendar_row<'a>(time: &str, title: &str, color: Color) -> Element<'a, Message> {
    row![
        text(time.to_string()).color(color).width(Length::Fixed(60.0)),
        text(truncate(title, 40)).color(color).width(Length::Fill),
    ]
    .spacing(8)
    .into()
}

fn github_row<'a>(n: &github::GitHubNotification) -> Element<'a, Message> {
    let icon = match n.type_.as_str() {
        "PullRequest" => "PR",
        "Issue" => "IS",
        "Release" => "RE",
        _ => "  ",
    };
    let repo = n
        .repo_name
        .rsplit('/')
        .next()
        .unwrap_or(&n.repo_name)
        .to_string();
    let r = row![
        text(icon).color(Color::from_rgb(0.55, 0.58, 0.6)),
        text(truncate(&repo, 15)).color(Color::from_rgb(0.55, 0.58, 0.6)).width(Length::Fixed(110.0)),
        text(truncate(&n.title, 45)).width(Length::Fill),
        text(github::time_ago(n.updated_at)).color(Color::from_rgb(0.55, 0.58, 0.6)),
    ]
    .spacing(8)
    .align_y(Vertical::Center);

    let url = n.html_url.clone();
    let id = n.id.clone();
    if !url.is_empty() {
        mouse_area(r)
            .on_press(Message::GithubOpen(url, id.clone()))
            .on_right_press(Message::GithubMarkRead(id))
            .into()
    } else {
        mouse_area(r).on_press(Message::GithubMarkRead(id)).into()
    }
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

fn cpu_stream() -> impl Stream<Item = Message> {
    iced::stream::channel(1, |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
        loop {
            let s = tokio::task::spawn_blocking(system::get_cpu_usage)
                .await
                .unwrap_or_else(|_| "🖥️ --".to_string());
            let _ = output.send(Message::Cpu(s)).await;
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    })
}

fn mem_stream() -> impl Stream<Item = Message> {
    iced::stream::channel(1, |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
        loop {
            let s = tokio::task::spawn_blocking(system::get_memory_usage)
                .await
                .unwrap_or_else(|_| "💾 --".to_string());
            let _ = output.send(Message::Mem(s)).await;
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    })
}

fn temp_stream() -> impl Stream<Item = Message> {
    iced::stream::channel(1, |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
        loop {
            let s = tokio::task::spawn_blocking(system::get_cpu_temperature)
                .await
                .unwrap_or_else(|_| "🌡️ --".to_string());
            let _ = output.send(Message::Temp(s)).await;
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    })
}

fn ping_stream() -> impl Stream<Item = Message> {
    iced::stream::channel(1, |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
        loop {
            let d = tokio::task::spawn_blocking(|| ping::perform_ping(PING_TARGET))
                .await
                .unwrap_or_default();
            let _ = output.send(Message::Ping(d)).await;
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    })
}

fn time_stream() -> impl Stream<Item = Message> {
    iced::stream::channel(1, |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
        loop {
            let s = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
            let _ = output.send(Message::Time(s)).await;
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
}

fn volume_stream() -> impl Stream<Item = Message> {
    iced::stream::channel(1, |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
        loop {
            let d = tokio::task::spawn_blocking(volume::update_volume)
                .await
                .unwrap_or_default();
            let _ = output.send(Message::Volume(d)).await;
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    })
}

fn kubectl_stream() -> impl Stream<Item = Message> {
    iced::stream::channel(1, |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
        loop {
            let d = tokio::task::spawn_blocking(kubectl::update_context)
                .await
                .unwrap_or_default();
            let _ = output.send(Message::Kubectl(d)).await;
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    })
}

fn battery_stream() -> impl Stream<Item = Message> {
    iced::stream::channel(1, |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
        loop {
            let s = tokio::task::spawn_blocking(battery::get_battery_status)
                .await
                .unwrap_or_else(|_| "🔋 --".to_string());
            let _ = output.send(Message::Battery(s)).await;
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    })
}

fn calendar_stream() -> impl Stream<Item = Message> {
    iced::stream::channel(1, |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
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
    })
}

fn github_stream() -> impl Stream<Item = Message> {
    iced::stream::channel(1, |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
        let token = match github::find_token() {
            Some(t) => t,
            None => {
                let _ = output
                    .send(Message::GitHub(github::GitHubData {
                        display_text: "GH ?".to_string(),
                        ..Default::default()
                    }))
                    .await;
                return;
            }
        };
        let mut gh = github::GitHub::new(token);
        loop {
            match gh.fetch().await {
                Ok(github::FetchResult::Data(d)) => {
                    let _ = output.send(Message::GitHub(d)).await;
                }
                Ok(github::FetchResult::NotModified) => {}
                Err(e) => log::warn!("github: {e}"),
            }
            tokio::time::sleep(Duration::from_secs(gh.poll_interval.max(1))).await;
        }
    })
}

fn stock_stream() -> impl Stream<Item = Message> {
    iced::stream::channel(1, |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
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
    })
}

fn spotify_stream() -> impl Stream<Item = Message> {
    iced::stream::channel(1, |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
        loop {
            let d = spotify::poll().await;
            let _ = output.send(Message::Spotify(d)).await;
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    })
}

fn claude_instances_stream() -> impl Stream<Item = Message> {
    iced::stream::channel(1, |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
        loop {
            let v = tokio::task::spawn_blocking(claude::instances)
                .await
                .unwrap_or_default();
            let _ = output.send(Message::ClaudeInstances(v)).await;
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    })
}

fn claude_block_stream() -> impl Stream<Item = Message> {
    iced::stream::channel(1, |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
        loop {
            let b = claude::block().await;
            let _ = output.send(Message::ClaudeBlock(b)).await;
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    })
}

fn claude_limits_stream() -> impl Stream<Item = Message> {
    iced::stream::channel(1, |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
        loop {
            let l = tokio::task::spawn_blocking(claude::limits).await.ok().flatten();
            let _ = output.send(Message::ClaudeLimits(l)).await;
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    })
}
