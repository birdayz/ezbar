//! A curated, single-family icon set for the bar — Nerd Font **Material Design**
//! glyphs (`nf-md-*`). One family means one stroke weight and one visual
//! language, so the bar reads as a *designed* set instead of a ransom note of
//! mixed FontAwesome / emoji / geometric marks (which is exactly what these
//! used to be).
//!
//! Requires a Nerd Font — the host defaults to `JetBrainsMono Nerd Font`. Each
//! glyph is a `\u{}` escape annotated with its upstream `md-*` name and
//! codepoint, so the family is auditable and can't silently drift back into a
//! mishmash. Verified to render (no tofu) against JetBrainsMono Nerd Font.

// ── system monitors ──────────────────────────────────────────────────────
pub const CPU: &str = "\u{f0ee0}"; // md-cpu-64-bit
pub const MEMORY: &str = "\u{f01bc}"; // md-database — a stack, kept visually distinct from the cpu chip
pub const TEMPERATURE: &str = "\u{f050f}"; // md-thermometer
pub const PING: &str = "\u{f04c5}"; // md-speedometer

// ── audio ────────────────────────────────────────────────────────────────
pub const VOLUME_HIGH: &str = "\u{f057e}"; // md-volume-high
pub const VOLUME_MEDIUM: &str = "\u{f0580}"; // md-volume-medium
pub const VOLUME_LOW: &str = "\u{f057f}"; // md-volume-low
pub const VOLUME_MUTE: &str = "\u{f075f}"; // md-volume-mute

// ── power ────────────────────────────────────────────────────────────────
pub const BATTERY: &str = "\u{f0079}"; // md-battery
pub const BATTERY_CHARGING: &str = "\u{f0084}"; // md-battery-charging
pub const BATTERY_ALERT: &str = "\u{f0083}"; // md-battery-alert

// ── media ────────────────────────────────────────────────────────────────
pub const SPOTIFY: &str = "\u{f04c7}"; // md-spotify
pub const PLAY: &str = "\u{f040a}"; // md-play
pub const PAUSE: &str = "\u{f03e4}"; // md-pause

// ── dev / cloud / agents ─────────────────────────────────────────────────
pub const GITHUB: &str = "\u{f02a4}"; // md-github
pub const KUBERNETES: &str = "\u{f10fe}"; // md-kubernetes
pub const ROBOT: &str = "\u{f06a9}"; // md-robot
pub const TIMER_SAND: &str = "\u{f051f}"; // md-timer-sand

// ── finance ──────────────────────────────────────────────────────────────
pub const TRENDING_UP: &str = "\u{f0535}"; // md-trending-up
pub const TRENDING_DOWN: &str = "\u{f0533}"; // md-trending-down
pub const TRENDING_FLAT: &str = "\u{f0534}"; // md-trending-neutral

// ── time / storage / net / system ────────────────────────────────────────
pub const CLOCK: &str = "\u{f0150}"; // md-clock-outline
pub const CALENDAR: &str = "\u{f0e17}"; // md-calendar-month
pub const DISK: &str = "\u{f02ca}"; // md-harddisk
pub const NET: &str = "\u{f04e2}"; // md-swap-vertical
pub const IP: &str = "\u{f0a60}"; // md-ip-network
pub const UPDATES: &str = "\u{f03d5}"; // md-package-up
pub const KEYBOARD: &str = "\u{f030c}"; // md-keyboard
