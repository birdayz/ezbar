//! `window_title` module: the focused window's title, from the shared sway service.
//! `[modules.window_title] max = 80` truncates long titles (0 = no limit).

use ezbar_plugin::iced::futures::{Stream, StreamExt};
use ezbar_plugin::iced::widget::text;
use ezbar_plugin::iced::{Element, Subscription};
use ezbar_plugin::{Ctx, ModMsg, Module, Response};

use crate::sources::sway;

struct Title(String);

pub struct WindowTitle {
    instance: u64,
    max: usize,
    title: String,
}

impl WindowTitle {
    pub fn new(instance: u64, cfg: &toml::Value) -> Self {
        let max = cfg
            .get("max")
            .and_then(|v| v.as_integer())
            .unwrap_or(80)
            .max(0) as usize;
        WindowTitle {
            instance,
            max,
            title: String::new(),
        }
    }
}

impl Module for WindowTitle {
    fn id(&self) -> &str {
        "window_title"
    }

    fn subscription(&self) -> Subscription<ModMsg> {
        ezbar_plugin::sub::keyed(self.instance, title_sub)
    }

    fn update(&mut self, msg: ModMsg) -> Response {
        if let Some(Title(t)) = msg.get::<Title>() {
            self.title = if self.max > 0 && t.chars().count() > self.max {
                let cut: String = t.chars().take(self.max.saturating_sub(1)).collect();
                format!("{cut}…")
            } else {
                t.clone()
            };
        }
        Response::none()
    }

    fn view(&self, _ctx: &Ctx) -> Element<'_, ModMsg> {
        text(self.title.clone()).into()
    }
}

fn title_sub(_id: &u64) -> impl Stream<Item = ModMsg> {
    sway::title().map(|t| ModMsg::new(Title(t)))
}
