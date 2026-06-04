//! Sway IPC **service**: one shared, event-driven connection that publishes a
//! `Snapshot` (workspaces + focused title + keyboard layout). Built-in modules
//! (workspaces, window_title, keyboard) subscribe to the slice they care about via
//! [`workspaces`]/[`title`]/[`layout`] instead of each opening their own sway socket;
//! [`run_command`] is the shared command path. One event loop, fanned to all.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use iced::futures::{SinkExt, Stream, StreamExt};
use iced::stream;
use swayipc::{Connection, Event, EventType, Node};
use tokio::sync::watch;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    pub name: String,
    /// focused on the focused output (the active workspace)
    pub focused: bool,
    /// visible on *some* output (focused, or active on another monitor)
    pub visible: bool,
    /// flagged urgent by a client
    pub urgent: bool,
}

/// The latest sway state the service publishes.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub workspaces: Vec<Workspace>,
    pub title: String,
    pub layout: String,
}

// ---------------------------------------------------------------------------
// The service: a lazily-started singleton, one event loop, a `watch` of Snapshot.
// ---------------------------------------------------------------------------

static BUS: OnceLock<watch::Sender<Arc<Snapshot>>> = OnceLock::new();

/// A receiver of the shared snapshot (starts the service on first use). `watch`
/// retains the latest value, so a late subscriber sees current state immediately.
fn bus() -> watch::Receiver<Arc<Snapshot>> {
    let tx = BUS.get_or_init(|| {
        let (tx, _rx) = watch::channel(Arc::new(Snapshot::default()));
        let tx2 = tx.clone();
        std::thread::spawn(move || loop {
            if let Err(e) = run_service(&tx2) {
                log::warn!("sway service: {e}");
            }
            std::thread::sleep(Duration::from_secs(1));
        });
        tx
    });
    tx.subscribe()
}

/// The current sway snapshot — a cheap clone of the latest value the background service
/// published (starts the service on first use). For the WASM read-only sway capability
/// (RFC 0013): the bar injects this into the reactor as a pull source.
pub fn snapshot() -> Arc<Snapshot> {
    bus().borrow().clone()
}

/// Subscribe to events once, re-query the changed slice, publish a fresh snapshot.
fn run_service(tx: &watch::Sender<Arc<Snapshot>>) -> swayipc::Fallible<()> {
    let mut q = Connection::new()?;
    let mut snap = Snapshot {
        workspaces: fetch_workspaces(&mut q)?,
        title: focused_title(&mut q),
        layout: active_layout(&mut q),
    };
    let _ = tx.send_replace(Arc::new(snap.clone()));

    let events = Connection::new()?.subscribe([
        EventType::Workspace,
        EventType::Window,
        EventType::Input,
    ])?;
    for event in events {
        match event? {
            Event::Workspace(_) => snap.workspaces = fetch_workspaces(&mut q)?,
            Event::Window(_) => snap.title = focused_title(&mut q),
            Event::Input(_) => snap.layout = active_layout(&mut q),
            _ => continue,
        }
        // send_replace keeps the latest even with zero current receivers
        let _ = tx.send_replace(Arc::new(snap.clone()));
    }
    Ok(())
}

/// Stream of snapshots (current value first, then on every change).
fn snapshots() -> impl Stream<Item = Arc<Snapshot>> {
    stream::channel(
        8,
        |mut out: iced::futures::channel::mpsc::Sender<Arc<Snapshot>>| async move {
            let mut rx = bus();
            let current = rx.borrow().clone(); // clone out so the Ref guard isn't held across await
            if out.send(current).await.is_err() {
                return;
            }
            while rx.changed().await.is_ok() {
                let snap = rx.borrow().clone();
                if out.send(snap).await.is_err() {
                    break;
                }
            }
        },
    )
}

/// Skip consecutive equal items (so a title change doesn't wake the workspaces module).
fn dedup<T: Clone + PartialEq + 'static>(s: impl Stream<Item = T>) -> impl Stream<Item = T> {
    let mut last: Option<T> = None;
    s.filter_map(move |item| {
        let keep = last.as_ref() != Some(&item);
        if keep {
            last = Some(item.clone());
        }
        async move { keep.then_some(item) }
    })
}

/// Workspace-list slice of the service (deduped).
pub fn workspaces() -> impl Stream<Item = Vec<Workspace>> {
    dedup(snapshots().map(|s| s.workspaces.clone()))
}

/// Focused-window-title slice (deduped).
pub fn title() -> impl Stream<Item = String> {
    dedup(snapshots().map(|s| s.title.clone()))
}

/// Active keyboard-layout slice (deduped).
pub fn layout() -> impl Stream<Item = String> {
    dedup(snapshots().map(|s| s.layout.clone()))
}

/// Run a sway command on a short-lived connection (shared command path).
pub fn run_command(cmd: impl Into<String>) {
    let cmd = cmd.into();
    std::thread::spawn(move || {
        if let Ok(mut c) = Connection::new() {
            let _ = c.run_command(cmd);
        }
    });
}

// ---------------------------------------------------------------------------
// Queries
// ---------------------------------------------------------------------------

fn sort_workspaces(ws: &mut [Workspace]) {
    ws.sort_by(
        |a, b| match (a.name.parse::<i32>(), b.name.parse::<i32>()) {
            (Ok(x), Ok(y)) => x.cmp(&y),
            _ => a.name.cmp(&b.name),
        },
    );
}

/// Snapshot the full workspace list (name + focused/visible/urgent state).
fn fetch_workspaces(conn: &mut Connection) -> swayipc::Fallible<Vec<Workspace>> {
    let mut state: Vec<Workspace> = conn
        .get_workspaces()?
        .into_iter()
        .map(|w| Workspace {
            name: w.name,
            focused: w.focused,
            visible: w.visible,
            urgent: w.urgent,
        })
        .collect();
    sort_workspaces(&mut state);
    Ok(state)
}

fn focused_title(conn: &mut Connection) -> String {
    conn.get_tree()
        .ok()
        .and_then(|t| focused_name(&t))
        .unwrap_or_default()
}

fn focused_name(node: &Node) -> Option<String> {
    if node.focused {
        return Some(node.name.clone().unwrap_or_default());
    }
    for c in node.nodes.iter().chain(node.floating_nodes.iter()) {
        if let Some(n) = focused_name(c) {
            return Some(n);
        }
    }
    None
}

/// The first keyboard's active layout, shortened (`English (US)` → `US`).
fn active_layout(conn: &mut Connection) -> String {
    conn.get_inputs()
        .ok()
        .and_then(|inputs| {
            inputs
                .into_iter()
                .find_map(|i| i.xkb_active_layout_name)
                .map(|name| short_layout(&name))
        })
        .unwrap_or_else(|| "??".to_string())
}

fn short_layout(name: &str) -> String {
    if let (Some(a), Some(b)) = (name.find('('), name.find(')')) {
        if b > a + 1 {
            return name[a + 1..b].to_string();
        }
    }
    name.split_whitespace().next().unwrap_or(name).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws(name: &str) -> Workspace {
        Workspace {
            name: name.into(),
            focused: false,
            visible: false,
            urgent: false,
        }
    }

    #[test]
    fn sorts_numerically_not_lexically() {
        let mut ws = vec![ws("10"), ws("2"), ws("1")];
        sort_workspaces(&mut ws);
        let names: Vec<&str> = ws.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, vec!["1", "2", "10"]);
    }

    #[test]
    fn named_workspaces_sort_as_strings() {
        let mut ws = vec![ws("web"), ws("code")];
        sort_workspaces(&mut ws);
        let names: Vec<&str> = ws.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, vec!["code", "web"]);
    }

    #[test]
    fn short_layout_extracts_paren_code() {
        assert_eq!(short_layout("English (US)"), "US");
        assert_eq!(short_layout("German"), "German");
    }
}
