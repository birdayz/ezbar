//! `keyboard` module: the active xkb layout (from the shared sway service); click
//! cycles to the next layout. `[modules.keyboard] icon = ""`.

use ezbar_plugin::iced::alignment::Vertical;
use ezbar_plugin::iced::futures::{Stream, StreamExt};
use ezbar_plugin::iced::widget::{mouse_area, row, text};
use ezbar_plugin::iced::{Element, Subscription};
use ezbar_plugin::{Ctx, ModMsg, Module, Response};

use crate::sources::sway;

enum Msg {
    Data(String),
    Switch,
}

pub struct Keyboard {
    instance: u64,
    icon: String,
    text: String,
}

impl Keyboard {
    pub fn new(instance: u64, cfg: &toml::Value) -> Self {
        let icon = cfg
            .get("icon")
            .and_then(|v| v.as_str())
            .unwrap_or("\u{f11c}") // keyboard
            .to_string();
        Keyboard {
            instance,
            icon,
            text: "??".to_string(),
        }
    }
}

impl Module for Keyboard {
    fn id(&self) -> &str {
        "keyboard"
    }

    fn subscription(&self) -> Subscription<ModMsg> {
        ezbar_plugin::sub::keyed(self.instance, layout_sub)
    }

    fn update(&mut self, msg: ModMsg) -> Response {
        match msg.get::<Msg>() {
            Some(Msg::Data(s)) => self.text = s.clone(),
            Some(Msg::Switch) => sway::run_command("input type:keyboard xkb_switch_layout next"),
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

fn layout_sub(_id: &u64) -> impl Stream<Item = ModMsg> {
    sway::layout().map(|l| ModMsg::new(Msg::Data(l)))
}
