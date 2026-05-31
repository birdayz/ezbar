//! `volume` module: level icon + %, click to mute, scroll to change.

use std::time::Duration;

use ezbar_plugin::iced::futures::{SinkExt, Stream};
use ezbar_plugin::iced::widget::{mouse_area, text};
use ezbar_plugin::iced::{Element, Subscription, Task};
use ezbar_plugin::{Ctx, ModMsg, Module, Response};

use crate::sources::volume::{self, VolumeData};

enum Msg {
    Data(VolumeData),
    Click,
    Scroll(i32),
}

pub struct Volume {
    instance: u64,
    data: VolumeData,
}

impl Volume {
    pub fn new(instance: u64) -> Self {
        Volume {
            instance,
            data: VolumeData::default(),
        }
    }
}

impl Module for Volume {
    fn id(&self) -> &str {
        "volume"
    }

    fn subscription(&self) -> Subscription<ModMsg> {
        ezbar_plugin::sub::keyed(self.instance, volume_stream)
    }

    fn update(&mut self, msg: ModMsg) -> Response {
        match msg.get::<Msg>() {
            Some(Msg::Data(d)) => {
                self.data = d.clone();
                Response::none()
            }
            Some(Msg::Click) => Response::task(Task::perform(
                async {
                    tokio::task::spawn_blocking(|| {
                        volume::toggle_mute();
                        volume::update_volume()
                    })
                    .await
                    .unwrap_or_default()
                },
                |d| ModMsg::new(Msg::Data(d)),
            )),
            Some(Msg::Scroll(dir)) => {
                let dir = *dir;
                Response::task(Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || {
                            volume::change_volume(dir);
                            volume::update_volume()
                        })
                        .await
                        .unwrap_or_default()
                    },
                    |d| ModMsg::new(Msg::Data(d)),
                ))
            }
            None => Response::none(),
        }
    }

    fn view(&self, _ctx: &Ctx) -> Element<'_, ModMsg> {
        mouse_area(text(self.data.string.clone()))
            .on_press(ModMsg::new(Msg::Click))
            .on_scroll(|delta| {
                let y = match delta {
                    ezbar_plugin::iced::mouse::ScrollDelta::Lines { y, .. } => y,
                    ezbar_plugin::iced::mouse::ScrollDelta::Pixels { y, .. } => y,
                };
                ModMsg::new(Msg::Scroll(if y > 0.0 { 1 } else { -1 }))
            })
            .into()
    }
}

fn volume_stream(_id: &u64) -> impl Stream<Item = ModMsg> {
    ezbar_plugin::iced::stream::channel(
        1,
        |mut out: ezbar_plugin::iced::futures::channel::mpsc::Sender<ModMsg>| async move {
            loop {
                let d = tokio::task::spawn_blocking(volume::update_volume)
                    .await
                    .unwrap_or_default();
                if out.send(ModMsg::new(Msg::Data(d))).await.is_err() {
                    break;
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        },
    )
}
