//! `net` module (RFC 0003): network throughput (down/up) from `/proc/net/dev`.
//! `[modules.net] interface = "" (auto, all non-lo)  interval = 2  icon = ""`.

use std::time::Duration;

use ezbar_plugin::iced::alignment::Vertical;
use ezbar_plugin::iced::futures::{SinkExt, Stream};
use ezbar_plugin::iced::widget::{row, text};
use ezbar_plugin::iced::{Element, Subscription};
use ezbar_plugin::{Ctx, ModMsg, Module, Response};

enum Msg {
    Data(String),
}

pub struct Net {
    instance: u64,
    interface: String,
    interval: u64,
    icon: String,
    text: String,
}

impl Net {
    pub fn new(instance: u64, cfg: &toml::Value) -> Self {
        let s = |k: &str| cfg.get(k).and_then(|v| v.as_str()).map(str::to_string);
        Net {
            instance,
            interface: s("interface").unwrap_or_default(),
            interval: cfg
                .get("interval")
                .and_then(|v| v.as_integer())
                .unwrap_or(2)
                .max(1) as u64,
            icon: s("icon").unwrap_or_else(|| "\u{f0ac}".to_string()), // globe
            text: " --".to_string(),
        }
    }
}

impl Module for Net {
    fn id(&self) -> &str {
        "net"
    }

    fn subscription(&self) -> Subscription<ModMsg> {
        Subscription::run_with(
            (self.instance, self.interface.clone(), self.interval),
            net_stream,
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

fn net_stream(data: &(u64, String, u64)) -> impl Stream<Item = ModMsg> {
    let (_, iface, interval) = data.clone();
    ezbar_plugin::iced::stream::channel(
        1,
        move |mut out: ezbar_plugin::iced::futures::channel::mpsc::Sender<ModMsg>| async move {
            let mut prev: Option<(u64, u64)> = None;
            loop {
                let ifc = iface.clone();
                let cur = ezbar_plugin::task::spawn_blocking(move || read_net(&ifc))
                    .await
                    .unwrap_or(None);
                let s = match (prev, cur) {
                    (Some((prx, ptx)), Some((rx, tx))) => {
                        let secs = interval.max(1);
                        let down = rx.saturating_sub(prx) / secs;
                        let up = tx.saturating_sub(ptx) / secs;
                        format!("\u{f063} {}  \u{f062} {}", human(down), human(up))
                    }
                    _ => " --".to_string(),
                };
                prev = cur;
                if out.send(ModMsg::new(Msg::Data(s))).await.is_err() {
                    break;
                }
                ezbar_plugin::task::sleep(Duration::from_secs(interval)).await;
            }
        },
    )
}

/// Sum rx/tx byte counters for the interface (`""` = all non-loopback).
fn read_net(iface: &str) -> Option<(u64, u64)> {
    let data = std::fs::read_to_string("/proc/net/dev").ok()?;
    let (mut rx, mut tx) = (0u64, 0u64);
    for line in data.lines().skip(2) {
        if let Some((name, rest)) = line.split_once(':') {
            let name = name.trim();
            if name == "lo" || (!iface.is_empty() && name != iface) {
                continue;
            }
            let cols: Vec<&str> = rest.split_whitespace().collect();
            if cols.len() >= 9 {
                rx += cols[0].parse::<u64>().unwrap_or(0);
                tx += cols[8].parse::<u64>().unwrap_or(0);
            }
        }
    }
    Some((rx, tx))
}

/// Bytes/sec as a short human string.
fn human(b: u64) -> String {
    const K: u64 = 1024;
    const M: u64 = 1024 * 1024;
    if b >= M {
        format!("{:.1}M", b as f64 / M as f64)
    } else if b >= K {
        format!("{:.0}K", b as f64 / K as f64)
    } else {
        format!("{b}B")
    }
}

#[cfg(test)]
mod tests {
    use super::human;

    #[test]
    fn human_scales_at_kib_and_mib_boundaries() {
        assert_eq!(human(0), "0B");
        assert_eq!(human(1023), "1023B"); // just under 1 KiB stays bytes
        assert_eq!(human(1024), "1K"); // exactly 1 KiB flips to K
        assert_eq!(human(1536), "2K"); // 1.5 KiB, K is integer-rounded
        assert_eq!(human(1024 * 1024 - 1), "1024K"); // just under 1 MiB
        assert_eq!(human(1024 * 1024), "1.0M"); // exactly 1 MiB flips to M, one decimal
        assert_eq!(human(1024 * 1024 * 3 / 2), "1.5M");
    }
}
