//! `clock` module: the date/time. `[modules.clock] format = "%Y-%m-%d %H:%M:%S"`
//! (chrono strftime).

use std::time::Duration;

use ezbar_plugin::iced::futures::{SinkExt, Stream};
use ezbar_plugin::iced::widget::text;
use ezbar_plugin::iced::{Element, Subscription};
use ezbar_plugin::{Ctx, ModMsg, Module, Response};

struct Tick(String);

pub struct Clock {
    instance: u64,
    format: String,
    text: String,
}

impl Clock {
    pub fn new(instance: u64, cfg: &toml::Value) -> Self {
        let format = cfg
            .get("format")
            .and_then(|v| v.as_str())
            .unwrap_or("%Y-%m-%d %H:%M:%S")
            .to_string();
        Clock {
            instance,
            format,
            text: String::new(),
        }
    }
}

impl Module for Clock {
    fn id(&self) -> &str {
        "clock"
    }

    fn subscription(&self) -> Subscription<ModMsg> {
        Subscription::run_with((self.instance, self.format.clone()), clock_stream)
    }

    fn update(&mut self, msg: ModMsg) -> Response {
        if let Some(Tick(s)) = msg.get::<Tick>() {
            self.text = s.clone();
        }
        Response::none()
    }

    fn view(&self, _ctx: &Ctx) -> Element<'_, ModMsg> {
        text(self.text.clone()).into()
    }
}

fn clock_stream(data: &(u64, String)) -> impl Stream<Item = ModMsg> {
    let fmt = data.1.clone();
    ezbar_plugin::iced::stream::channel(
        1,
        move |mut out: ezbar_plugin::iced::futures::channel::mpsc::Sender<ModMsg>| async move {
            // Re-render only when the *rendered* string changes, never on a wake
            // that produces the identical text. With a seconds format that's still
            // 1/s (unavoidable); with `%H:%M` it's 1/min — 60× fewer full-bar
            // relayouts for a clock that didn't visibly move.
            let mut last: Option<String> = None;
            loop {
                let s = chrono::Local::now().format(&fmt).to_string();
                if last.as_deref() != Some(s.as_str()) {
                    last = Some(s.clone());
                    if out.send(ModMsg::new(Tick(s))).await.is_err() {
                        break;
                    }
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        },
    )
}
