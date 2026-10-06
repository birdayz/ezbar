//! Shared, subscription-owned tray service. One bus/X11 host for all outputs and
//! tray instances; removing the last subscription releases both protocol owners.
//! No shell helpers, application commands, or downloaded code are executed.

mod icon;
mod sni;
mod xembed;

pub use icon::{Icon, Pixels};
pub use sni::MenuNode;

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex, OnceLock, Weak,
};
use tokio::sync::{mpsc, oneshot, watch};

pub const MAX_ITEMS: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ItemId {
    Sni { owner: String, path: String },
    Xembed(u32),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub id: ItemId,
    pub title: String,
    pub icon: Option<Icon>,
    pub passive: bool,
    pub attention: bool,
    pub is_menu: bool,
    pub menu: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub sni: Vec<Item>,
    pub xembed: Vec<Item>,
}

#[derive(Clone, Copy, Debug)]
pub enum Action {
    Activate,
    Secondary,
    ContextMenu,
    Scroll(i32, bool), // delta in degrees; true = horizontal
    MenuClick(i32),
}

struct Command {
    id: ItemId,
    action: Action,
    position: (i32, i32),
    menu: Option<String>,
    reply: Option<oneshot::Sender<Vec<MenuNode>>>,
}

pub struct Service {
    state: watch::Receiver<Snapshot>,
    sni: mpsc::Sender<Command>,
    xembed: std::sync::mpsc::SyncSender<Command>,
    stop: Arc<AtomicBool>,
    bus_task: tokio::task::JoinHandle<()>,
}

impl Service {
    pub fn acquire() -> Arc<Self> {
        static SHARED: OnceLock<Mutex<Weak<Service>>> = OnceLock::new();
        let mut weak = SHARED
            .get_or_init(|| Mutex::new(Weak::new()))
            .lock()
            .unwrap();
        if let Some(service) = weak.upgrade() {
            return service;
        }
        let (updates, state) = watch::channel(Snapshot::default());
        let (sni, commands) = mpsc::channel(64);
        let (xembed, xcommands) = std::sync::mpsc::sync_channel(64);
        let stop = Arc::new(AtomicBool::new(false));
        let xstop = stop.clone();
        let xupdates = updates.clone();
        // Dedicated worker: synchronous X11 replies never block iced or Tokio.
        if let Err(error) = std::thread::Builder::new()
            .name("ezbar-xembed".into())
            .spawn(move || {
                xembed::run(xcommands, xupdates, xstop);
            })
        {
            log::warn!("could not start tray XEmbed worker: {error}");
        }
        let bus_task = tokio::spawn(sni::run(commands, updates));
        let service = Arc::new(Self {
            state,
            sni,
            xembed,
            stop,
            bus_task,
        });
        *weak = Arc::downgrade(&service);
        service
    }

    pub fn subscribe(&self) -> watch::Receiver<Snapshot> {
        self.state.clone()
    }

    pub fn action(&self, item: &Item, action: Action, position: (i32, i32)) {
        let command = Command {
            id: item.id.clone(),
            action,
            position,
            menu: item.menu.clone(),
            reply: None,
        };
        // Bounded, non-blocking queues. A misbehaving app cannot stall input/rendering.
        match item.id {
            ItemId::Sni { .. } => {
                let _ = self.sni.try_send(command);
            }
            ItemId::Xembed(_) => {
                let _ = self.xembed.try_send(command);
            }
        }
    }

    pub async fn menu(&self, item: &Item) -> Vec<MenuNode> {
        let (reply, result) = oneshot::channel();
        let command = Command {
            id: item.id.clone(),
            action: Action::ContextMenu,
            position: (0, 0),
            menu: item.menu.clone(),
            reply: Some(reply),
        };
        if self.sni.try_send(command).is_err() {
            return Vec::new();
        }
        tokio::time::timeout(std::time::Duration::from_secs(3), result)
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default()
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.bus_task.abort();
    }
}

fn publish(updates: &watch::Sender<Snapshot>, items: Vec<Item>, xembed: bool) {
    updates.send_if_modified(|state| {
        let current = if xembed {
            &mut state.xembed
        } else {
            &mut state.sni
        };
        if *current == items {
            false
        } else {
            *current = items;
            true
        }
    });
}
