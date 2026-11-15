//! `inhibit` module: a click-to-toggle **idle inhibitor**.
//!
//! While on, automatic idle actions (screen blank, lock, suspend-on-idle) are held off; turn
//! it off and idle behaves as normal. It takes a systemd-logind inhibitor lock
//! (`org.freedesktop.login1.Manager.Inhibit`, exactly what `systemd-inhibit(1)` does), so it
//! works with idle daemons that honour logind inhibitors (swayidle, hypridle, …) and needs no
//! extra process. The lock is an fd: dropping it releases the lock, and the kernel does so even
//! if ezbar is killed, so the bar can never leave the machine stuck awake.
//!
//! ```toml
//! [modules.inhibit]
//! icon_on  = ""    # shown while inhibiting (accent colour)
//! icon_off = ""    # shown while idle is allowed (dimmed)
//! what     = "idle" # logind lock type(s), colon-separated: "idle", "idle:sleep", …
//! ```
//!
//! A failed attempt (no system bus, logind refusing) paints the icon in the `warn` colour and
//! logs the reason; the next click retries.

use std::os::fd::OwnedFd;
use std::sync::{Arc, Mutex};

use ezbar_plugin::iced::widget::{mouse_area, text};
use ezbar_plugin::iced::{Element, Task};
use ezbar_plugin::{Ctx, ModMsg, Module, Reconfigure, Response};

const DEFAULT_ICON_ON: &str = "\u{f06e}"; // eye
const DEFAULT_ICON_OFF: &str = "\u{f070}"; // eye-slash
const DEFAULT_WHAT: &str = "idle";

type AcquireResult = Result<OwnedFd, String>;

enum Msg {
    Toggle,
    /// The logind reply. The fd is moved out of the slot exactly once; if the message is never
    /// delivered (instance retired meanwhile) dropping the slot closes the fd, releasing the lock.
    Acquired(Arc<Mutex<Option<AcquireResult>>>),
}

pub struct Inhibit {
    icon_on: String,
    icon_off: String,
    what: String,
    /// The held logind lock; `Some` ⇔ idle is currently inhibited.
    lock: Option<OwnedFd>,
    /// An `Inhibit` call is in flight — further clicks are ignored until it answers.
    pending: bool,
    /// The last attempt failed (cleared by the next click).
    failed: bool,
}

struct Settings {
    icon_on: String,
    icon_off: String,
    what: String,
}

fn parse(cfg: &toml::Value) -> Settings {
    let s = |k: &str, d: &str| {
        cfg.get(k)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or(d)
            .to_string()
    };
    Settings {
        icon_on: s("icon_on", DEFAULT_ICON_ON),
        icon_off: s("icon_off", DEFAULT_ICON_OFF),
        what: s("what", DEFAULT_WHAT),
    }
}

impl Inhibit {
    pub fn new(cfg: &toml::Value) -> Self {
        let c = parse(cfg);
        Inhibit {
            icon_on: c.icon_on,
            icon_off: c.icon_off,
            what: c.what,
            lock: None,
            pending: false,
            failed: false,
        }
    }

    /// A click. Returns the lock type to acquire when an `Inhibit` call should start;
    /// `None` when nothing needs to be requested (releasing, or one is already in flight).
    fn on_toggle(&mut self) -> Option<String> {
        if self.pending {
            return None;
        }
        self.failed = false;
        if self.lock.take().is_some() {
            return None; // dropping the fd released the lock
        }
        self.pending = true;
        Some(self.what.clone())
    }

    /// The logind answer for a request started by [`on_toggle`](Self::on_toggle).
    fn on_acquired(&mut self, result: AcquireResult) {
        self.pending = false;
        match result {
            Ok(fd) => self.lock = Some(fd),
            Err(e) => {
                log::warn!("inhibit: could not take the idle inhibitor: {e}");
                self.failed = true;
            }
        }
    }
}

impl Module for Inhibit {
    fn id(&self) -> &str {
        "inhibit"
    }

    fn update(&mut self, msg: ModMsg) -> Response {
        match msg.get::<Msg>() {
            Some(Msg::Toggle) => {
                if let Some(what) = self.on_toggle() {
                    return Response::task(Task::perform(acquire(what), |r| {
                        ModMsg::new(Msg::Acquired(Arc::new(Mutex::new(Some(r)))))
                    }));
                }
            }
            Some(Msg::Acquired(slot)) => {
                let result = slot
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take()
                    .unwrap_or_else(|| Err("reply already consumed".into()));
                self.on_acquired(result);
            }
            None => {}
        }
        Response::none()
    }

    fn view(&self, ctx: &Ctx) -> Element<'_, ModMsg> {
        let (icon, color) = if self.failed {
            (&self.icon_off, ctx.warn())
        } else if self.lock.is_some() {
            (&self.icon_on, ctx.accent())
        } else {
            (&self.icon_off, ctx.fg_dim())
        };
        mouse_area(text(icon.clone()).color(color))
            .on_press(ModMsg::new(Msg::Toggle))
            .into()
    }

    fn reconfigure(&mut self, cfg: &toml::Value) -> Reconfigure {
        let c = parse(cfg);
        self.icon_on = c.icon_on;
        self.icon_off = c.icon_off;
        if c.what != self.what {
            // A lock of the old type no longer matches the config: release it (idle is allowed
            // again until the next click) rather than keep holding something not asked for.
            self.what = c.what;
            self.lock = None;
        }
        Reconfigure::Applied { resubscribe: false }
    }

    fn shutdown(&mut self) {
        self.lock = None; // releases the logind lock
    }
}

/// Ask logind for an inhibitor lock and return its fd. Runs on the host's tokio executor
/// (a `Task` from `update`), never on the render thread.
async fn acquire(what: String) -> AcquireResult {
    let conn = zbus::Connection::system()
        .await
        .map_err(|e| format!("system bus: {e}"))?;
    let reply = conn
        .call_method(
            Some("org.freedesktop.login1"),
            "/org/freedesktop/login1",
            Some("org.freedesktop.login1.Manager"),
            "Inhibit",
            // (what, who, why, mode) — "block" so idle daemons actually hold off
            &(
                what.as_str(),
                "ezbar",
                "Idle inhibition requested from the bar",
                "block",
            ),
        )
        .await
        .map_err(|e| format!("logind Inhibit({what}): {e}"))?;
    let fd: zbus::zvariant::OwnedFd = reply
        .body()
        .deserialize()
        .map_err(|e| format!("logind Inhibit reply: {e}"))?;
    Ok(fd.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;

    fn make(cfg: &str) -> Inhibit {
        Inhibit::new(&cfg.parse::<toml::Value>().unwrap())
    }

    /// A real, owned fd standing in for the logind lock.
    fn fake_lock() -> OwnedFd {
        std::fs::File::open("/dev/null").unwrap().into()
    }

    fn fd_is_open(raw: i32) -> bool {
        // fcntl(F_GETFD) fails with EBADF on a closed descriptor.
        unsafe { libc::fcntl(raw, libc::F_GETFD) != -1 }
    }

    #[test]
    fn defaults_and_overrides() {
        let d = make("");
        assert_eq!(d.what, "idle");
        assert_eq!(d.icon_on, DEFAULT_ICON_ON);
        let c = make("icon_on = \"ON\"\nicon_off = \"OFF\"\nwhat = \"idle:sleep\"");
        assert_eq!((c.icon_on.as_str(), c.icon_off.as_str()), ("ON", "OFF"));
        assert_eq!(c.what, "idle:sleep");
        // a blank value falls back to the default rather than producing an empty glyph/lock type
        assert_eq!(make("what = \"  \"").what, "idle");
    }

    #[test]
    fn click_requests_a_lock_then_the_next_click_releases_it() {
        let mut m = make("what = \"idle:sleep\"");
        assert_eq!(m.on_toggle().as_deref(), Some("idle:sleep"));
        assert!(m.pending && m.lock.is_none());

        m.on_acquired(Ok(fake_lock()));
        assert!(!m.pending && m.lock.is_some());

        assert_eq!(m.on_toggle(), None); // releasing needs no request
        assert!(m.lock.is_none());
    }

    #[test]
    fn releasing_closes_the_fd() {
        let mut m = make("");
        let fd = fake_lock();
        let raw = fd.as_raw_fd();
        m.on_toggle();
        m.on_acquired(Ok(fd));
        assert!(fd_is_open(raw));
        m.on_toggle();
        assert!(!fd_is_open(raw), "dropping the lock must close its fd");
    }

    #[test]
    fn clicks_while_a_request_is_in_flight_are_ignored() {
        let mut m = make("");
        assert!(m.on_toggle().is_some());
        assert_eq!(m.on_toggle(), None);
        assert!(m.pending, "the in-flight request must not be cancelled");
    }

    #[test]
    fn failure_marks_the_icon_and_the_next_click_retries() {
        let mut m = make("");
        m.on_toggle();
        m.on_acquired(Err("no bus".into()));
        assert!(m.failed && !m.pending && m.lock.is_none());
        assert!(m.on_toggle().is_some(), "a failed attempt can be retried");
        assert!(!m.failed, "the next click clears the failure mark");
    }

    #[test]
    fn changing_what_releases_a_held_lock_but_icons_keep_it() {
        let mut m = make("");
        m.on_toggle();
        m.on_acquired(Ok(fake_lock()));
        let same = "what = \"idle\"\nicon_on = \"X\""
            .parse::<toml::Value>()
            .unwrap();
        assert_eq!(
            m.reconfigure(&same),
            Reconfigure::Applied { resubscribe: false }
        );
        assert!(m.lock.is_some(), "cosmetic edits must not drop the lock");
        assert_eq!(m.icon_on, "X");
        let other = "what = \"idle:sleep\"".parse::<toml::Value>().unwrap();
        m.reconfigure(&other);
        assert!(m.lock.is_none());
        assert_eq!(m.what, "idle:sleep");
    }
}
