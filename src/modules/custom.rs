//! `custom` module (RFC 0003): a no-code, command-driven widget.
//!
//! Two forms — **poll** or **stream**:
//! ```toml
//! # poll: run `command` every `interval` seconds, show its trimmed stdout
//! [modules.custom]
//! command  = "checkupdates | wc -l"
//! interval = 300
//! icon     = ""          # a Nerd-Font glyph (themed with the accent colour)
//! on_click = "alacritty -e paru -Syu"
//!
//! # stream: run a long-running `listen_cmd` once; each stdout LINE updates the chip
//! # (event-driven, no polling — the widget changes the instant the command emits a line).
//! # If both are set, `listen_cmd` wins. The process is restarted (gently) if it exits.
//! [modules.weather-stream]
//! listen_cmd = "my-daemon --watch"   # e.g. a script that prints a value per change
//! icon       = ""
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
    /// A long-running command streamed line-by-line (RFC 0003). When set it supersedes
    /// `command`/`interval` (the widget is event-driven off the command's stdout).
    listen_cmd: Option<String>,
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
            listen_cmd: s("listen_cmd").filter(|c| !c.trim().is_empty()),
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
        // Bake the command(s) into the recipe data so the fn-ptr stream can read them, and
        // so a config change re-rolls the recipe (RFC 0002 generation). `listen_cmd` (stream)
        // supersedes `command` (poll) when set.
        match &self.listen_cmd {
            Some(cmd) => Subscription::run_with((self.instance, cmd.clone()), listen_stream),
            None => Subscription::run_with(
                (self.instance, self.command.clone(), self.interval),
                custom_stream,
            ),
        }
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

/// Stream a long-running command: spawn it once and emit each stdout LINE as it arrives
/// (event-driven — no polling). If the process exits, it's respawned after a short backoff
/// (so a crashing `listen_cmd` recovers). `kill_on_drop` ensures the child dies when the
/// subscription is re-rolled on a config change, instead of leaking.
fn listen_stream(data: &(u64, String)) -> impl Stream<Item = ModMsg> {
    let (_, cmd) = data.clone();
    ezbar_plugin::iced::stream::channel(
        1,
        move |mut out: ezbar_plugin::iced::futures::channel::mpsc::Sender<ModMsg>| async move {
            use tokio::io::AsyncBufReadExt;
            loop {
                let child = tokio::process::Command::new("sh")
                    .arg("-c")
                    .arg(&cmd)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::piped())
                    .kill_on_drop(true)
                    .spawn();
                let mut child = match child {
                    Ok(c) => c,
                    Err(e) => {
                        log::warn!("custom: listen_cmd spawn failed: {e}");
                        ezbar_plugin::task::sleep(Duration::from_secs(5)).await;
                        continue;
                    }
                };
                if let Some(stdout) = child.stdout.take() {
                    let mut lines = tokio::io::BufReader::new(stdout).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        if out
                            .send(ModMsg::new(Msg::Data(line.trim().to_string())))
                            .await
                            .is_err()
                        {
                            return; // module dropped → stop (kill_on_drop reaps the child)
                        }
                    }
                }
                let _ = child.wait().await; // reap, then respawn after a gentle backoff
                ezbar_plugin::task::sleep(Duration::from_secs(2)).await;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn make(cfg: &str) -> Custom {
        Custom::new(0, "custom", &cfg.parse::<toml::Value>().unwrap())
    }

    #[test]
    fn listen_cmd_parsed_when_present() {
        let c = make("listen_cmd = \"my-daemon --watch\"");
        assert_eq!(c.listen_cmd.as_deref(), Some("my-daemon --watch"));
    }

    #[test]
    fn listen_cmd_absent_or_blank_is_none() {
        // unset → poll form (command path), not stream
        assert!(make("command = \"date\"\ninterval = 9").listen_cmd.is_none());
        // a blank/whitespace value doesn't accidentally select the (empty) stream form
        assert!(make("listen_cmd = \"   \"").listen_cmd.is_none());
    }
}
