//! Audio volume via PulseAudio (pactl) with ALSA (amixer) fallback.
//! Port of pkg/datasource/volume.go.

use std::process::Command;

#[allow(dead_code)] // volume/is_muted mirror the Go model
#[derive(Debug, Clone)]
pub struct VolumeData {
    pub volume: i32,
    pub string: String,
    pub is_muted: bool,
}

impl Default for VolumeData {
    fn default() -> Self {
        VolumeData {
            volume: 0,
            string: "--".to_string(),
            is_muted: false,
        }
    }
}

pub fn update_volume() -> VolumeData {
    let (volume, is_muted) = get_volume_info();
    VolumeData {
        volume,
        string: format_volume(volume, is_muted),
        is_muted,
    }
}

fn format_volume(volume: i32, is_muted: bool) -> String {
    if is_muted {
        return "--%".to_string();
    }
    // the speaker icon is now a separate SVG widget (chosen by level in the module)
    format!("{}%", volume)
}

fn get_volume_info() -> (i32, bool) {
    if let Some(v) = get_pulseaudio_volume() {
        return v;
    }
    if let Some(v) = get_alsa_volume() {
        return v;
    }
    (0, false)
}

fn get_pulseaudio_volume() -> Option<(i32, bool)> {
    let output = Command::new("pactl")
        .args(["get-sink-volume", "@DEFAULT_SINK@"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines() {
        if line.contains("Volume:") {
            let parts: Vec<&str> = line.split('/').collect();
            if parts.len() >= 2 {
                let vp = parts[1].trim().trim_end_matches('%').trim();
                if let Ok(volume) = vp.parse::<i32>() {
                    let mute = Command::new("pactl")
                        .args(["get-sink-mute", "@DEFAULT_SINK@"])
                        .output();
                    let is_muted = match mute {
                        Ok(m) => String::from_utf8_lossy(&m.stdout).contains("Mute: yes"),
                        Err(_) => false,
                    };
                    return Some((volume, is_muted));
                }
            }
        }
    }
    None
}

fn get_alsa_volume() -> Option<(i32, bool)> {
    let output = Command::new("amixer")
        .args(["get", "Master"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines() {
        if line.contains('[') && line.contains('%') {
            let start = line.find('[');
            let end = line.find("%]");
            if let (Some(s), Some(e)) = (start, end) {
                if let Ok(volume) = line[s + 1..e].parse::<i32>() {
                    let is_muted = line.contains("[off]");
                    return Some((volume, is_muted));
                }
            }
        }
    }
    None
}

pub fn toggle_mute() {
    let res = Command::new("pactl")
        .args(["set-sink-mute", "@DEFAULT_SINK@", "toggle"])
        .status();
    if res.map(|s| !s.success()).unwrap_or(true) {
        let _ = Command::new("amixer")
            .args(["set", "Master", "toggle"])
            .status();
    }
}

/// direction: +1 to raise, -1 to lower (by 5%).
pub fn change_volume(direction: i32) {
    let change = direction * 5;
    let sign = if change > 0 { "+" } else { "" };
    let pulse = Command::new("pactl")
        .args([
            "set-sink-volume",
            "@DEFAULT_SINK@",
            &format!("{}{}%", sign, change),
        ])
        .status();
    if pulse.map(|s| !s.success()).unwrap_or(true) {
        let alsa_dir = if change > 0 {
            format!("{}%+", change)
        } else {
            format!("{}%-", -change)
        };
        let _ = Command::new("amixer")
            .args(["set", "Master", &alsa_dir])
            .status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_by_level() {
        assert_eq!(format_volume(0, false), "0%");
        assert_eq!(format_volume(20, false), "20%");
        assert_eq!(format_volume(40, false), "40%");
        assert_eq!(format_volume(80, false), "80%");
        assert_eq!(format_volume(50, true), "--%");
    }
}
