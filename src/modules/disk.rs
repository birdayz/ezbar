//! `disk` module (RFC 0003): usage of a mount point — icon + capacity percent.
//! `[modules.disk] path = "/"  interval = 30  icon = ""`.

use std::process::Command;
use std::time::Duration;

use ezbar_plugin::iced::alignment::Vertical;
use ezbar_plugin::iced::futures::{SinkExt, Stream};
use ezbar_plugin::iced::widget::{row, text};
use ezbar_plugin::iced::{Element, Subscription};
use ezbar_plugin::{Ctx, ModMsg, Module, Response};

enum Msg {
    Data(String),
}

pub struct Disk {
    instance: u64,
    path: String,
    interval: u64,
    icon: String,
    text: String,
}

impl Disk {
    pub fn new(instance: u64, cfg: &toml::Value) -> Self {
        let s = |k: &str| cfg.get(k).and_then(|v| v.as_str()).map(str::to_string);
        Disk {
            instance,
            path: s("path").unwrap_or_else(|| "/".to_string()),
            interval: cfg
                .get("interval")
                .and_then(|v| v.as_integer())
                .unwrap_or(30)
                .max(1) as u64,
            icon: s("icon").unwrap_or_else(|| "\u{f0a0}".to_string()), // hdd
            text: " --".to_string(),
        }
    }
}

impl Module for Disk {
    fn id(&self) -> &str {
        "disk"
    }

    fn subscription(&self) -> Subscription<ModMsg> {
        Subscription::run_with(
            (self.instance, self.path.clone(), self.interval),
            disk_stream,
        )
    }

    fn update(&mut self, msg: ModMsg) -> Response {
        if let Some(Msg::Data(s)) = msg.get::<Msg>() {
            self.text = s.clone();
        }
        Response::none()
    }

    fn view(&self, ctx: &Ctx) -> Element<'_, ModMsg> {
        row(vec![
            text(self.icon.clone()).color(ctx.accent()).into(),
            text(self.text.clone()).into(),
        ])
        .spacing(4)
        .align_y(Vertical::Center)
        .into()
    }
}

fn disk_stream(data: &(u64, String, u64)) -> impl Stream<Item = ModMsg> {
    let (_, path, interval) = data.clone();
    ezbar_plugin::iced::stream::channel(
        1,
        move |mut out: ezbar_plugin::iced::futures::channel::mpsc::Sender<ModMsg>| async move {
            loop {
                let p = path.clone();
                let s = ezbar_plugin::task::spawn_blocking(move || disk_usage(&p))
                    .await
                    .unwrap_or_else(|_| " --".to_string());
                if out.send(ModMsg::new(Msg::Data(s))).await.is_err() {
                    break;
                }
                ezbar_plugin::task::sleep(Duration::from_secs(interval)).await;
            }
        },
    )
}

/// `df -P <path>`: row 2, column 5 is the capacity percent.
fn disk_usage(path: &str) -> String {
    let out = match Command::new("df").arg("-P").arg(path).output() {
        Ok(o) => o,
        Err(_) => return " --".to_string(),
    };
    let text = String::from_utf8_lossy(&out.stdout);
    if let Some(line) = text.lines().nth(1) {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() >= 5 {
            return format!(" {}", cols[4]);
        }
    }
    " --".to_string()
}
