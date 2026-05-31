//! `keyboard` module (RFC 0003): the active xkb layout via sway IPC; click cycles
//! to the next layout. `[modules.keyboard] interval = 2  icon = ""`.

use std::time::Duration;

use ezbar_plugin::iced::alignment::Vertical;
use ezbar_plugin::iced::futures::{SinkExt, Stream};
use ezbar_plugin::iced::widget::{mouse_area, row, text};
use ezbar_plugin::iced::{Element, Subscription};
use ezbar_plugin::{Ctx, ModMsg, Module, Response};

enum Msg {
    Data(String),
    Switch,
}

pub struct Keyboard {
    instance: u64,
    interval: u64,
    icon: String,
    text: String,
}

impl Keyboard {
    pub fn new(instance: u64, cfg: &toml::Value) -> Self {
        let s = |k: &str| cfg.get(k).and_then(|v| v.as_str()).map(str::to_string);
        Keyboard {
            instance,
            interval: cfg
                .get("interval")
                .and_then(|v| v.as_integer())
                .unwrap_or(2)
                .max(1) as u64,
            icon: s("icon").unwrap_or_else(|| "\u{f11c}".to_string()), // keyboard
            text: "??".to_string(),
        }
    }
}

impl Module for Keyboard {
    fn id(&self) -> &str {
        "keyboard"
    }

    fn subscription(&self) -> Subscription<ModMsg> {
        Subscription::run_with((self.instance, self.interval), keyboard_stream)
    }

    fn update(&mut self, msg: ModMsg) -> Response {
        match msg.get::<Msg>() {
            Some(Msg::Data(s)) => self.text = s.clone(),
            Some(Msg::Switch) => {
                std::thread::spawn(|| {
                    if let Ok(mut c) = swayipc::Connection::new() {
                        let _ = c.run_command("input type:keyboard xkb_switch_layout next");
                    }
                });
            }
            None => {}
        }
        Response::none()
    }

    fn view(&self, ctx: &Ctx) -> Element<'_, ModMsg> {
        let content = row(vec![
            text(self.icon.clone()).color(ctx.accent()).into(),
            text(self.text.clone()).into(),
        ])
        .spacing(4)
        .align_y(Vertical::Center);
        mouse_area(content)
            .on_press(ModMsg::new(Msg::Switch))
            .into()
    }
}

fn keyboard_stream(data: &(u64, u64)) -> impl Stream<Item = ModMsg> {
    let interval = data.1;
    ezbar_plugin::iced::stream::channel(
        1,
        move |mut out: ezbar_plugin::iced::futures::channel::mpsc::Sender<ModMsg>| async move {
            loop {
                let s = ezbar_plugin::task::spawn_blocking(active_layout)
                    .await
                    .unwrap_or_else(|_| "??".to_string());
                if out.send(ModMsg::new(Msg::Data(s))).await.is_err() {
                    break;
                }
                ezbar_plugin::task::sleep(Duration::from_secs(interval)).await;
            }
        },
    )
}

/// The first keyboard's active layout, shortened (`English (US)` → `US`).
fn active_layout() -> String {
    let mut conn = match swayipc::Connection::new() {
        Ok(c) => c,
        Err(_) => return "??".to_string(),
    };
    let inputs = match conn.get_inputs() {
        Ok(i) => i,
        Err(_) => return "??".to_string(),
    };
    for input in inputs {
        if let Some(name) = input.xkb_active_layout_name {
            return short_layout(&name);
        }
    }
    "??".to_string()
}

fn short_layout(name: &str) -> String {
    if let (Some(a), Some(b)) = (name.find('('), name.find(')')) {
        if b > a + 1 {
            return name[a + 1..b].to_string();
        }
    }
    name.split_whitespace().next().unwrap_or(name).to_string()
}
