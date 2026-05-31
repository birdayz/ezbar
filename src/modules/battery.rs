//! `battery` module: level icon + %. Hides itself on a machine with no battery.

use std::time::Duration;

use ezbar_plugin::iced::futures::{SinkExt, Stream};
use ezbar_plugin::iced::widget::text;
use ezbar_plugin::iced::{Element, Subscription};
use ezbar_plugin::{Ctx, ModMsg, Module, Response};

use crate::sources::battery;

struct Status(String);

pub struct Battery {
    instance: u64,
    present: bool,
    text: String,
}

impl Battery {
    pub fn new(instance: u64) -> Self {
        Battery {
            instance,
            present: battery::has_battery(),
            text: " --".to_string(),
        }
    }
}

impl Module for Battery {
    fn id(&self) -> &str {
        "battery"
    }

    fn visible(&self) -> bool {
        self.present
    }

    fn subscription(&self) -> Subscription<ModMsg> {
        if !self.present {
            return Subscription::none();
        }
        ezbar_plugin::sub::keyed(self.instance, battery_stream)
    }

    fn update(&mut self, msg: ModMsg) -> Response {
        if let Some(Status(s)) = msg.get::<Status>() {
            self.text = s.clone();
        }
        Response::none()
    }

    fn view(&self, _ctx: &Ctx) -> Element<'_, ModMsg> {
        text(self.text.clone()).into()
    }
}

fn battery_stream(_id: &u64) -> impl Stream<Item = ModMsg> {
    ezbar_plugin::iced::stream::channel(
        1,
        |mut out: ezbar_plugin::iced::futures::channel::mpsc::Sender<ModMsg>| async move {
            loop {
                let s = tokio::task::spawn_blocking(battery::get_battery_status)
                    .await
                    .unwrap_or_else(|_| " --".to_string());
                if out.send(ModMsg::new(Status(s))).await.is_err() {
                    break;
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        },
    )
}
