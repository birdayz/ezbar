//! `ping` module: latency to a target + GPU sparkline, click toggles the graph.
//! `[modules.ping] target = "8.8.8.8"`.

use std::time::Duration;

use ezbar_plugin::iced::alignment::Vertical;
use ezbar_plugin::iced::futures::{SinkExt, Stream};
use ezbar_plugin::iced::widget::{canvas, mouse_area, row, text};
use ezbar_plugin::iced::{Element, Length, Subscription};
use ezbar_plugin::ui::graph::{Graph, GraphKind};
use ezbar_plugin::{Ctx, ModMsg, Module, Response};

use crate::history::History;
use crate::sources::ping::{self, PingData};

enum Msg {
    Data(PingData),
    Toggle,
}

pub struct Ping {
    instance: u64,
    target: String,
    data: PingData,
    hist: History,
    show_graph: bool,
}

impl Ping {
    pub fn new(instance: u64, cfg: &toml::Value) -> Self {
        let target = cfg
            .get("target")
            .and_then(|v| v.as_str())
            .unwrap_or("8.8.8.8")
            .to_string();
        Ping {
            instance,
            target,
            data: PingData::default(),
            hist: History::new(40),
            show_graph: false,
        }
    }
}

impl Module for Ping {
    fn id(&self) -> &str {
        "ping"
    }

    fn subscription(&self) -> Subscription<ModMsg> {
        // bake the target into the recipe so a config change re-rolls the stream
        Subscription::run_with((self.instance, self.target.clone()), ping_stream)
    }

    fn update(&mut self, msg: ModMsg) -> Response {
        match msg.get::<Msg>() {
            Some(Msg::Data(d)) => {
                if d.is_up {
                    self.hist.add(d.latency);
                }
                self.data = d.clone();
            }
            Some(Msg::Toggle) => self.show_graph = !self.show_graph,
            None => {}
        }
        Response::none()
    }

    fn view(&self, _ctx: &Ctx) -> Element<'_, ModMsg> {
        let lbl = mouse_area(text(self.data.string.clone())).on_press(ModMsg::new(Msg::Toggle));
        if self.show_graph {
            let g = canvas(Graph {
                values: self.hist.ordered(),
                kind: GraphKind::Ping,
            })
            .width(Length::Fixed(80.0))
            .height(Length::Fixed(20.0));
            row(vec![lbl.into(), g.into()])
                .spacing(4)
                .align_y(Vertical::Center)
                .into()
        } else {
            lbl.into()
        }
    }
}

fn ping_stream(data: &(u64, String)) -> impl Stream<Item = ModMsg> {
    let target = data.1.clone();
    ezbar_plugin::iced::stream::channel(
        1,
        move |mut out: ezbar_plugin::iced::futures::channel::mpsc::Sender<ModMsg>| async move {
            loop {
                let t = target.clone();
                let d = tokio::task::spawn_blocking(move || ping::perform_ping(&t))
                    .await
                    .unwrap_or_default();
                if out.send(ModMsg::new(Msg::Data(d))).await.is_err() {
                    break;
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        },
    )
}
