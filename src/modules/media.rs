//! `media` module (Tier-B): now-playing from any MPRIS player via `playerctl`. Click toggles
//! play/pause, scroll skips tracks; the chip hides itself when nothing is playing.
//!
//! ```toml
//! [modules.media]
//! max_len = 40   # truncate "artist – title" past this many chars (default 40)
//! ```

use std::time::Duration;

use ezbar_plugin::iced::alignment::Vertical;
use ezbar_plugin::iced::futures::{SinkExt, Stream};
use ezbar_plugin::iced::mouse::ScrollDelta;
use ezbar_plugin::iced::widget::{mouse_area, row, text, Space};
use ezbar_plugin::iced::{Element, Subscription, Task};
use ezbar_plugin::{Ctx, ModMsg, Module, Response};

use crate::sources::media::{self, MediaData};

enum Msg {
    Data(MediaData),
    PlayPause,
    Skip(i32),
}

pub struct Media {
    instance: u64,
    data: MediaData,
    max_len: usize,
}

impl Media {
    pub fn new(instance: u64, cfg: &toml::Value) -> Self {
        let max_len = cfg
            .get("max_len")
            .and_then(|v| v.as_integer())
            .unwrap_or(40)
            .clamp(8, 200) as usize;
        Media {
            instance,
            data: MediaData::default(),
            max_len,
        }
    }
}

impl Module for Media {
    fn id(&self) -> &str {
        "media"
    }

    fn subscription(&self) -> Subscription<ModMsg> {
        ezbar_plugin::sub::keyed(self.instance, media_stream)
    }

    fn update(&mut self, msg: ModMsg) -> Response {
        match msg.get::<Msg>() {
            Some(Msg::Data(d)) => self.data = d.clone(),
            // toggle + read fresh state back so the glyph flips immediately, not next poll
            Some(Msg::PlayPause) => {
                return Response::task(Task::perform(
                    async { ezbar_plugin::task::spawn_blocking(media::play_pause).await },
                    |d| ModMsg::new(Msg::Data(d.unwrap_or_default())),
                ));
            }
            Some(Msg::Skip(dir)) => {
                let dir = *dir;
                ezbar_plugin::task::spawn_blocking(move || {
                    if dir > 0 {
                        media::next();
                    } else {
                        media::previous();
                    }
                });
            }
            None => {}
        }
        Response::none()
    }

    fn view(&self, ctx: &Ctx) -> Element<'_, ModMsg> {
        if !self.data.active {
            return Space::new().into(); // nothing playing → an empty (hidden) chip
        }
        // show the action the click performs: a pause glyph while playing, a play glyph while paused.
        let glyph = if self.data.playing {
            "\u{f04c}" //
        } else {
            "\u{f04b}" //
        };
        let content = row(vec![
            text(glyph).color(ctx.accent()).into(),
            text(now_playing(&self.data, self.max_len)).into(),
        ])
        .spacing(6)
        .align_y(Vertical::Center);
        mouse_area(content)
            .on_press(ModMsg::new(Msg::PlayPause))
            .on_scroll(|d| {
                let y = match d {
                    ScrollDelta::Lines { y, .. } | ScrollDelta::Pixels { y, .. } => y,
                };
                ModMsg::new(Msg::Skip(if y > 0.0 { 1 } else { -1 }))
            })
            .into()
    }
}

/// `"artist – title"` (or just the title), truncated to `max` chars with an ellipsis.
fn now_playing(d: &MediaData, max: usize) -> String {
    let s = if d.artist.is_empty() {
        d.title.clone()
    } else {
        format!("{} \u{2013} {}", d.artist, d.title)
    };
    if s.chars().count() > max {
        format!(
            "{}\u{2026}",
            s.chars().take(max.saturating_sub(1)).collect::<String>()
        )
    } else {
        s
    }
}

fn media_stream(_id: &u64) -> impl Stream<Item = ModMsg> {
    ezbar_plugin::iced::stream::channel(
        1,
        |mut out: ezbar_plugin::iced::futures::channel::mpsc::Sender<ModMsg>| async move {
            // Re-render only when the now-playing state changes (not every 2s tick) — an idle
            // or unchanged player costs zero relayouts.
            let mut last = MediaData::default();
            loop {
                let d = ezbar_plugin::task::spawn_blocking(media::get)
                    .await
                    .unwrap_or_default();
                if d != last {
                    last = d.clone();
                    if out.send(ModMsg::new(Msg::Data(d))).await.is_err() {
                        break;
                    }
                }
                ezbar_plugin::task::sleep(Duration::from_secs(2)).await;
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::now_playing;
    use crate::sources::media::MediaData;

    #[test]
    fn now_playing_formats_and_truncates() {
        let d = MediaData {
            active: true,
            playing: true,
            artist: "A".into(),
            title: "Song".into(),
            player: "p".into(),
        };
        assert_eq!(now_playing(&d, 40), "A \u{2013} Song"); // "artist – title", no truncation
        let long = MediaData {
            artist: "VeryLongArtistName".into(),
            title: "AndAnEvenLongerSongTitle".into(),
            ..d.clone()
        };
        let out = now_playing(&long, 10);
        assert_eq!(out.chars().count(), 10); // clamped to max
        assert!(out.ends_with('\u{2026}')); // …with an ellipsis
        let t = MediaData {
            artist: String::new(),
            title: "JustTitle".into(),
            ..d
        };
        assert_eq!(now_playing(&t, 40), "JustTitle"); // no artist → title only
    }
}
