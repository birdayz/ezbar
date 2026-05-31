//! `ip` module (RFC 0003): the primary outbound IP address.
//! `[modules.ip] interval = 30  icon = ""`.

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

pub struct Ip {
    instance: u64,
    interval: u64,
    icon: String,
    text: String,
}

impl Ip {
    pub fn new(instance: u64, cfg: &toml::Value) -> Self {
        let s = |k: &str| cfg.get(k).and_then(|v| v.as_str()).map(str::to_string);
        Ip {
            instance,
            interval: cfg
                .get("interval")
                .and_then(|v| v.as_integer())
                .unwrap_or(30)
                .max(1) as u64,
            icon: s("icon").unwrap_or_else(|| "\u{f0ac}".to_string()), // globe
            text: " --".to_string(),
        }
    }
}

impl Module for Ip {
    fn id(&self) -> &str {
        "ip"
    }

    fn subscription(&self) -> Subscription<ModMsg> {
        Subscription::run_with((self.instance, self.interval), ip_stream)
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

fn ip_stream(data: &(u64, u64)) -> impl Stream<Item = ModMsg> {
    let interval = data.1;
    ezbar_plugin::iced::stream::channel(
        1,
        move |mut out: ezbar_plugin::iced::futures::channel::mpsc::Sender<ModMsg>| async move {
            loop {
                let s = ezbar_plugin::task::spawn_blocking(primary_ip)
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

/// The `src` address `ip route get` chooses toward the internet.
fn primary_ip() -> String {
    let out = match Command::new("ip")
        .args(["route", "get", "8.8.8.8"])
        .output()
    {
        Ok(o) => o,
        Err(_) => return " --".to_string(),
    };
    let text = String::from_utf8_lossy(&out.stdout);
    if let Some(pos) = text.find("src ") {
        if let Some(ip) = text[pos + 4..].split_whitespace().next() {
            return ip.to_string();
        }
    }
    "no-ip".to_string()
}
