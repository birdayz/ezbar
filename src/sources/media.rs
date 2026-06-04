//! Now-playing from any MPRIS player, via `playerctl` (Tier-B `media`). We shell out rather
//! than open a D-Bus connection — `playerctl` is the standard tool and keeps this dependency-
//! free (the proper shared `Service`/D-Bus layer is a separate, larger lift, RFC 0003).

use std::process::Command;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MediaData {
    /// a player exists and has a track (else the chip hides itself)
    pub active: bool,
    /// currently playing (vs paused/stopped) — drives the play/pause glyph
    pub playing: bool,
    pub artist: String,
    pub title: String,
    pub player: String,
}

/// Read the active player's status/metadata. `title` is placed LAST in the format because it
/// commonly contains the `|` we split on (e.g. a YouTube title), so `splitn(4)` keeps it whole.
pub fn get() -> MediaData {
    let out = Command::new("playerctl")
        .args([
            "metadata",
            "--format",
            "{{status}}|{{artist}}|{{playerName}}|{{title}}",
        ])
        .output();
    match out {
        Ok(o) if o.status.success() => parse(&String::from_utf8_lossy(&o.stdout)),
        _ => MediaData::default(), // no playerctl, or no player running
    }
}

fn parse(s: &str) -> MediaData {
    let s = s.trim();
    if s.is_empty() {
        return MediaData::default();
    }
    let mut it = s.splitn(4, '|');
    let status = it.next().unwrap_or("");
    let artist = it.next().unwrap_or("").trim().to_string();
    let player = it.next().unwrap_or("").trim().to_string();
    let title = it.next().unwrap_or("").trim().to_string();
    MediaData {
        active: !(title.is_empty() && artist.is_empty()),
        playing: status.eq_ignore_ascii_case("Playing"),
        artist,
        title,
        player,
    }
}

fn run(cmd: &str) {
    let _ = Command::new("playerctl").arg(cmd).status();
}

/// Toggle play/pause on the active player, then read the fresh state back (for a snappy
/// chip update instead of waiting for the next poll).
pub fn play_pause() -> MediaData {
    run("play-pause");
    get()
}
pub fn next() {
    run("next");
}
pub fn previous() {
    run("previous");
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn title_with_pipes_stays_whole() {
        // a YouTube-style title full of `|` must survive (it's the last field)
        let d = parse("Playing|Karo|firefox|Kirby (Stage 5) | Episode 22 (100% | 4K)");
        assert!(d.active && d.playing);
        assert_eq!(d.artist, "Karo");
        assert_eq!(d.player, "firefox");
        assert_eq!(d.title, "Kirby (Stage 5) | Episode 22 (100% | 4K)");
    }

    #[test]
    fn paused_and_empty() {
        assert_eq!(parse(""), Default::default()); // no player → inactive
        let p = parse("Paused|Artist|spotify|Song");
        assert!(p.active && !p.playing); // a paused track is still active
        assert!(!parse("Stopped|||").active); // no artist AND no title → inactive
    }
}
