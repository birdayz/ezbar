//! `custom` module (RFC 0003): a no-code, command-driven widget. Runs a shell
//! `command` every `interval` seconds and shows its (trimmed) stdout next to an
//! optional `icon`; an optional `on_click` command runs when clicked.
//!
//! ```toml
//! [modules.custom]
//! command  = "checkupdates | wc -l"
//! interval = 300
//! icon     = ""          # a Nerd-Font glyph (themed with the accent colour)
//! on_click = "alacritty -e paru -Syu"
//! ```

use std::process::Command;
use std::time::Duration;

use ezbar_plugin::iced::alignment::Vertical;
use ezbar_plugin::iced::futures::{SinkExt, Stream};
use ezbar_plugin::iced::widget::{mouse_area, row, text};
use ezbar_plugin::iced::{Element, Subscription};
use ezbar_plugin::{Ctx, ModMsg, Module, Response};

enum Msg {
    Data(String),
    Click(String),
}

pub struct Custom {
    instance: u64,
    name: String,
    command: String,
    interval: u64,
    icon: String,
    on_click: Option<String>,
    text: String,
}

impl Custom {
    /// Build from `[modules.<name>]` config. `name` is the placement id so the host
    /// finds this instance when rendering the zone.
    pub fn new(instance: u64, name: &str, cfg: &toml::Value) -> Self {
        let s = |k: &str| cfg.get(k).and_then(|v| v.as_str()).map(str::to_string);
        Custom {
            instance,
            name: name.to_string(),
            command: s("command").unwrap_or_default(),
            interval: cfg
                .get("interval")
                .and_then(|v| v.as_integer())
                .unwrap_or(5)
                .max(1) as u64,
            icon: s("icon").unwrap_or_default(),
            on_click: s("on_click"),
            text: String::new(),
        }
    }
}

impl Module for Custom {
    fn id(&self) -> &str {
        &self.name
    }

    fn subscription(&self) -> Subscription<ModMsg> {
        // Bake command + interval into the recipe data so the fn-ptr stream can read
        // them, and so a config change re-rolls the recipe (RFC 0002 generation).
        Subscription::run_with(
            (self.instance, self.command.clone(), self.interval),
            custom_stream,
        )
    }

    fn update(&mut self, msg: ModMsg) -> Response {
        match msg.get::<Msg>() {
            Some(Msg::Data(s)) => self.text = s.clone(),
            Some(Msg::Click(cmd)) => {
                let cmd = cmd.clone();
                // fire-and-forget; never block update()
                std::thread::spawn(move || {
                    let _ = Command::new("sh").arg("-c").arg(&cmd).status();
                });
            }
            None => {}
        }
        Response::none()
    }

    fn view(&self, ctx: &Ctx) -> Element<'_, ModMsg> {
        let mut parts: Vec<Element<ModMsg>> = Vec::new();
        if !self.icon.is_empty() {
            parts.push(text(self.icon.clone()).color(ctx.accent()).into());
        }
        parts.push(text(self.text.clone()).into());
        let content = row(parts).spacing(4).align_y(Vertical::Center);
        match &self.on_click {
            Some(cmd) => mouse_area(content)
                .on_press(ModMsg::new(Msg::Click(cmd.clone())))
                .into(),
            None => content.into(),
        }
    }
}

fn custom_stream(data: &(u64, String, u64)) -> impl Stream<Item = ModMsg> {
    let (_, command, interval) = data.clone();
    ezbar_plugin::iced::stream::channel(
        1,
        move |mut out: ezbar_plugin::iced::futures::channel::mpsc::Sender<ModMsg>| async move {
            loop {
                let c = command.clone();
                let s = ezbar_plugin::task::spawn_blocking(move || run_command(&c))
                    .await
                    .unwrap_or_default();
                if out.send(ModMsg::new(Msg::Data(s))).await.is_err() {
                    break;
                }
                ezbar_plugin::task::sleep(Duration::from_secs(interval)).await;
            }
        },
    )
}

/// Run a shell command and return its trimmed stdout (empty on any failure).
fn run_command(cmd: &str) -> String {
    if cmd.trim().is_empty() {
        return String::new();
    }
    Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}
