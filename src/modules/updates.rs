//! `updates` module (RFC 0003): pending package count from a `check_cmd` (one line
//! per update); click runs `update_cmd`.
//! `[modules.updates] check_cmd = "checkupdates"  update_cmd = "alacritty -e paru -Syu"  interval = 3600  icon = ""`.

use std::process::Command;
use std::time::Duration;

use ezbar_plugin::iced::alignment::Vertical;
use ezbar_plugin::iced::futures::{SinkExt, Stream};
use ezbar_plugin::iced::widget::{mouse_area, row, text};
use ezbar_plugin::iced::{Element, Subscription};
use ezbar_plugin::{Ctx, ModMsg, Module, Response};

enum Msg {
    Data(usize),
    Click(String),
}

pub struct Updates {
    instance: u64,
    check_cmd: String,
    update_cmd: Option<String>,
    interval: u64,
    icon: String,
    count: usize,
}

impl Updates {
    pub fn new(instance: u64, cfg: &toml::Value) -> Self {
        let s = |k: &str| cfg.get(k).and_then(|v| v.as_str()).map(str::to_string);
        Updates {
            instance,
            check_cmd: s("check_cmd").unwrap_or_else(|| "checkupdates".to_string()),
            update_cmd: s("update_cmd"),
            interval: cfg
                .get("interval")
                .and_then(|v| v.as_integer())
                .unwrap_or(3600)
                .max(1) as u64,
            icon: s("icon").unwrap_or_else(|| "\u{f0ed}".to_string()), // cloud-download
            count: 0,
        }
    }
}

impl Module for Updates {
    fn id(&self) -> &str {
        "updates"
    }

    fn subscription(&self) -> Subscription<ModMsg> {
        Subscription::run_with(
            (self.instance, self.check_cmd.clone(), self.interval),
            updates_stream,
        )
    }

    fn update(&mut self, msg: ModMsg) -> Response {
        match msg.get::<Msg>() {
            Some(Msg::Data(n)) => self.count = *n,
            Some(Msg::Click(cmd)) => {
                let cmd = cmd.clone();
                std::thread::spawn(move || {
                    let _ = Command::new("sh").arg("-c").arg(&cmd).status();
                });
            }
            None => {}
        }
        Response::none()
    }

    fn view(&self, ctx: &Ctx) -> Element<'_, ModMsg> {
        let color = if self.count > 0 {
            ctx.warn()
        } else {
            ctx.fg_dim()
        };
        let content = row(vec![
            text(self.icon.clone()).color(color).into(),
            text(format!("{}", self.count)).into(),
        ])
        .spacing(4)
        .align_y(Vertical::Center);
        match &self.update_cmd {
            Some(cmd) => mouse_area(content)
                .on_press(ModMsg::new(Msg::Click(cmd.clone())))
                .into(),
            None => content.into(),
        }
    }
}

fn updates_stream(data: &(u64, String, u64)) -> impl Stream<Item = ModMsg> {
    let (_, check_cmd, interval) = data.clone();
    ezbar_plugin::iced::stream::channel(
        1,
        move |mut out: ezbar_plugin::iced::futures::channel::mpsc::Sender<ModMsg>| async move {
            loop {
                let c = check_cmd.clone();
                let n = ezbar_plugin::task::spawn_blocking(move || count_updates(&c))
                    .await
                    .unwrap_or(0);
                if out.send(ModMsg::new(Msg::Data(n))).await.is_err() {
                    break;
                }
                ezbar_plugin::task::sleep(Duration::from_secs(interval)).await;
            }
        },
    )
}

/// Non-empty stdout lines of `check_cmd` = update count (0 on any failure).
fn count_updates(cmd: &str) -> usize {
    if cmd.trim().is_empty() {
        return 0;
    }
    Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .output()
        .ok()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter(|l| !l.trim().is_empty())
                .count()
        })
        .unwrap_or(0)
}
