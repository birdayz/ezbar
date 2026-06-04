//! `temperature` module: CPU temp label + GPU sparkline, click toggles the graph.

use std::time::Duration;

use ezbar_plugin::iced::alignment::Vertical;
use ezbar_plugin::iced::futures::{SinkExt, Stream};
use ezbar_plugin::iced::widget::{mouse_area, row, text};
use ezbar_plugin::iced::{Element, Subscription};
use ezbar_plugin::icons::Icon;
use ezbar_plugin::ui::graph::GraphKind;
use ezbar_plugin::{Ctx, ModMsg, Module, Response};

use crate::history::History;
use crate::sources::system;

enum Msg {
    Data(String),
    Toggle,
}

pub struct Temperature {
    instance: u64,
    text: String,
    hist: History,
    show_graph: bool,
    gcfg: crate::modules::GraphCfg,
}

impl Temperature {
    pub fn new(instance: u64, cfg: &toml::Value) -> Self {
        let gcfg = crate::modules::graph_cfg(cfg, 60);
        Temperature {
            instance,
            text: " --".to_string(),
            hist: History::new(gcfg.samples),
            show_graph: true,
            gcfg,
        }
    }
}

impl Module for Temperature {
    fn id(&self) -> &str {
        "temperature"
    }

    fn subscription(&self) -> Subscription<ModMsg> {
        ezbar_plugin::sub::keyed(self.instance, temp_stream)
    }

    fn update(&mut self, msg: ModMsg) -> Response {
        match msg.get::<Msg>() {
            Some(Msg::Data(s)) => {
                self.hist.add(system::extract_temperature_value(s));
                self.text = s.clone();
            }
            Some(Msg::Toggle) => self.show_graph = !self.show_graph,
            None => {}
        }
        Response::none()
    }

    fn view(&self, ctx: &Ctx) -> Element<'_, ModMsg> {
        let lbl = mouse_area(
            row(vec![
                Icon::Temperature.view(ctx.theme.text_size, ctx.fg()),
                text(self.text.clone()).into(),
            ])
            .spacing(5)
            .align_y(Vertical::Center),
        )
        .on_press(ModMsg::new(Msg::Toggle));
        if self.show_graph {
            let g = crate::modules::graph_widget(
                &self.gcfg,
                GraphKind::Temperature,
                self.hist.ordered(),
                ctx.graph_paint(self.gcfg.line_color.as_deref()),
            );
            row(vec![lbl.into(), g])
                .spacing(4)
                .align_y(Vertical::Center)
                .into()
        } else {
            lbl.into()
        }
    }
}

fn temp_stream(_id: &u64) -> impl Stream<Item = ModMsg> {
    ezbar_plugin::iced::stream::channel(
        1,
        |mut out: ezbar_plugin::iced::futures::channel::mpsc::Sender<ModMsg>| async move {
            loop {
                let s = tokio::task::spawn_blocking(system::get_cpu_temperature)
                    .await
                    .unwrap_or_else(|_| " --".to_string());
                if out.send(ModMsg::new(Msg::Data(s))).await.is_err() {
                    break;
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        },
    )
}
