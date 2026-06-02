//! `spotify` module: now-playing with a marquee for long titles; click to
//! play/pause (or authorize), scroll to skip tracks.

use std::time::Duration;

use ezbar_plugin::iced::alignment::Vertical;
use ezbar_plugin::iced::futures::{SinkExt, Stream};
use ezbar_plugin::iced::widget::{mouse_area, row, text};
use ezbar_plugin::iced::{Element, Subscription, Task};
use ezbar_plugin::icons::Icon;
use ezbar_plugin::{Ctx, ModMsg, Module, Response};

use crate::sources::spotify::{self, SpotifyData};

enum Msg {
    Data(SpotifyData),
    Tick,
    Click,
    Scroll(bool),
}

pub struct Spotify {
    instance: u64,
    data: SpotifyData,
    offset: usize,
}

impl Spotify {
    pub fn new(instance: u64) -> Self {
        Spotify {
            instance,
            data: SpotifyData::default(),
            offset: 0,
        }
    }
}

impl Module for Spotify {
    fn id(&self) -> &str {
        "spotify"
    }

    fn subscription(&self) -> Subscription<ModMsg> {
        Subscription::batch([
            ezbar_plugin::sub::keyed(self.instance, sp_stream),
            ezbar_plugin::iced::time::every(Duration::from_millis(500))
                .map(|_| ModMsg::new(Msg::Tick)),
        ])
    }

    fn update(&mut self, msg: ModMsg) -> Response {
        match msg.get::<Msg>() {
            Some(Msg::Data(d)) => {
                self.data = d.clone();
                Response::none()
            }
            Some(Msg::Tick) => {
                self.offset = self.offset.wrapping_add(1);
                Response::none()
            }
            Some(Msg::Click) => {
                let needs_auth = self.data.needs_auth;
                let is_playing = self.data.is_playing;
                Response::task(Task::perform(
                    async move {
                        if needs_auth {
                            let _ = tokio::task::spawn_blocking(spotify::authorize).await;
                        } else {
                            spotify::toggle_playback(is_playing).await;
                        }
                        spotify::poll().await
                    },
                    |d| ModMsg::new(Msg::Data(d)),
                ))
            }
            Some(Msg::Scroll(next)) => {
                let next = *next;
                Response::task(Task::perform(
                    async move {
                        spotify::skip(next).await;
                        spotify::poll().await
                    },
                    |d| ModMsg::new(Msg::Data(d)),
                ))
            }
            None => Response::none(),
        }
    }

    fn view(&self, ctx: &Ctx) -> Element<'_, ModMsg> {
        mouse_area(
            row(vec![
                Icon::Spotify.view(ctx.theme.text_size, ctx.fg()),
                text(marquee(&self.data.track_string, self.offset, 40)).into(),
            ])
            .spacing(5)
            .align_y(Vertical::Center),
        )
        .on_press(ModMsg::new(Msg::Click))
        .on_scroll(|delta| {
            let y = match delta {
                ezbar_plugin::iced::mouse::ScrollDelta::Lines { y, .. } => y,
                ezbar_plugin::iced::mouse::ScrollDelta::Pixels { y, .. } => y,
            };
            ModMsg::new(Msg::Scroll(y > 0.0))
        })
        .into()
    }
}

fn sp_stream(_id: &u64) -> impl Stream<Item = ModMsg> {
    ezbar_plugin::iced::stream::channel(
        1,
        |mut out: ezbar_plugin::iced::futures::channel::mpsc::Sender<ModMsg>| async move {
            loop {
                let d = spotify::poll().await;
                if out.send(ModMsg::new(Msg::Data(d))).await.is_err() {
                    break;
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        },
    )
}

/// Scroll long titles: returns a `max_len`-char window that advances with `offset`.
fn marquee(s: &str, offset: usize, max_len: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max_len {
        return s.to_string();
    }
    let padded: Vec<char> = s.chars().chain("    ".chars()).collect();
    let n = padded.len();
    let start = offset % n;
    (0..max_len).map(|i| padded[(start + i) % n]).collect()
}

#[cfg(test)]
mod tests {
    use super::marquee;

    #[test]
    fn marquee_short_unchanged() {
        assert_eq!(marquee("hi", 5, 40), "hi");
    }

    #[test]
    fn marquee_long_rotates() {
        let s: String = (b'a'..=b'z').map(|c| c as char).collect();
        let a = marquee(&s, 0, 10);
        let b = marquee(&s, 3, 10);
        assert_eq!(a.chars().count(), 10);
        assert_ne!(a, b);
    }
}
