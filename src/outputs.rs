//! Named Wayland outputs from the same connection used by iced_layershell.
//!
//! Sway advertises wl_output v4, whose Name/Done events give us a deterministic readiness gate.
//! A surface must use `OutputOption::Output(outputs.get(name)?)`, never OutputName: the latter
//! silently falls back to the compositor's default output while its separate name cache lags.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex, Weak};

use tokio::sync::watch;
use wayland_client::protocol::{wl_callback, wl_output, wl_registry};
use wayland_client::{ConnectError, Connection, Dispatch, DispatchError, Proxy, QueueHandle};

#[derive(Debug)]
pub enum Error {
    Connect(ConnectError),
    Dispatch(DispatchError),
    Thread(std::io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connect(e) => write!(f, "connect Wayland outputs: {e}"),
            Self::Dispatch(e) => write!(f, "initialize Wayland outputs: {e}"),
            Self::Thread(e) => write!(f, "start Wayland output watcher: {e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Connect(e) => Some(e),
            Self::Dispatch(e) => Some(e),
            Self::Thread(e) => Some(e),
        }
    }
}

#[derive(Clone)]
pub struct Outputs {
    shared: Arc<Shared>,
}

struct Shared {
    connection: Connection,
    cache: Mutex<OutputCache<wl_output::WlOutput>>,
    changes: watch::Receiver<u64>,
    qh: QueueHandle<State>,
}

impl Outputs {
    pub fn connect() -> Result<Self, Error> {
        Self::from_connection(Connection::connect_to_env().map_err(Error::Connect)?)
    }

    fn from_connection(connection: Connection) -> Result<Self, Error> {
        let mut queue = connection.new_event_queue();
        let qh = queue.handle();
        let (changes, receiver) = watch::channel(0);
        let shared = Arc::new(Shared {
            connection,
            cache: Mutex::new(OutputCache::default()),
            changes: receiver,
            qh: qh.clone(),
        });
        let mut state = State {
            shared: Arc::downgrade(&shared),
            changes,
        };
        let registry = shared.connection.display().get_registry(&qh, ());
        // The first roundtrip advertises globals and binds outputs; the second receives each
        // bound output's initial Name/Done. Hotplug readiness is handled by the queue below.
        queue.roundtrip(&mut state).map_err(Error::Dispatch)?;
        queue.roundtrip(&mut state).map_err(Error::Dispatch)?;
        std::thread::Builder::new()
            .name("wayland-outputs".into())
            .spawn(move || {
                // Each thread may blocking_dispatch its own queue on a shared Connection;
                // wayland-client coordinates socket reads and routes events to their queues.
                while state.shared.strong_count() != 0 {
                    if let Err(e) = queue.blocking_dispatch(&mut state) {
                        log::warn!("outputs: Wayland dispatch: {e}");
                        if let Some(shared) = state.shared.upgrade() {
                            release(shared.cache.lock().unwrap().clear());
                            state.notify();
                        }
                        break;
                    }
                }
                drop(registry);
                // State owns the only watch sender: failure/shutdown closes subscribers after
                // they observe the last invalidation, rather than leaving them waiting forever.
            })
            .map_err(Error::Thread)?;
        Ok(Self { shared })
    }

    /// Pass this clone to iced's Settings.with_connection, not a second connect_to_env().
    pub fn connection(&self) -> Connection {
        self.shared.connection.clone()
    }

    /// None means absent or not named/ready yet. Removed globals are never returned, even if
    /// their already-queued Name/Done events are dispatched later.
    pub fn get(&self, name: &str) -> Option<wl_output::WlOutput> {
        self.shared
            .cache
            .lock()
            .unwrap()
            .get(name)
            .filter(|output| output.is_alive())
            .cloned()
    }

    /// Reconcile after changed().await, plus an initial kick when starting the subscription.
    /// Values are generations (only changes matter); bursts may coalesce safely.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.shared.changes.clone()
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        if let Ok(cache) = self.cache.get_mut() {
            release(cache.clear());
        }
        // Wake the queue's blocking_dispatch so its Weak owner check can stop the watcher.
        self.connection.display().sync(&self.qh, ());
        let _ = self.connection.flush();
    }
}

fn release(outputs: Vec<wl_output::WlOutput>) {
    for output in outputs {
        if output.is_alive() {
            output.release();
        }
    }
}

struct State {
    shared: Weak<Shared>,
    changes: watch::Sender<u64>,
}

impl State {
    fn notify(&self) {
        self.changes
            .send_modify(|generation| *generation = generation.wrapping_add(1));
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let Some(shared) = state.shared.upgrade() else {
            return;
        };
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } if interface == "wl_output" => {
                if version < 4 {
                    // No guess/default binding on older protocols: ezbar targets modern Sway.
                    log::warn!("outputs: wl_output {name} has version {version}, need v4 names");
                    return;
                }
                let output = registry.bind::<wl_output::WlOutput, _, _>(name, 4, qh, name);
                shared.cache.lock().unwrap().insert(name, output);
            }
            wl_registry::Event::GlobalRemove { name } => {
                let removed = shared.cache.lock().unwrap().remove(name);
                if let Some(output) = removed {
                    release(vec![output]);
                    state.notify();
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_output::WlOutput, u32> for State {
    fn event(
        state: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        global: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(shared) = state.shared.upgrade() else {
            return;
        };
        let mut cache = shared.cache.lock().unwrap();
        match event {
            wl_output::Event::Name { name } => cache.name(*global, name),
            wl_output::Event::Done if cache.done(*global) => state.notify(),
            _ => {}
        }
    }
}

wayland_client::delegate_noop!(State: ignore wl_callback::WlCallback);

/// Generic proxy storage lets lifecycle invariants be tested without opening a desktop.
struct OutputCache<P> {
    entries: BTreeMap<u32, Entry<P>>,
}

struct Entry<P> {
    proxy: P,
    pending_name: Option<String>,
    ready_name: Option<String>,
}

impl<P> Default for OutputCache<P> {
    fn default() -> Self {
        Self {
            entries: BTreeMap::new(),
        }
    }
}

impl<P> OutputCache<P> {
    fn insert(&mut self, global: u32, proxy: P) {
        self.entries.insert(
            global,
            Entry {
                proxy,
                pending_name: None,
                ready_name: None,
            },
        );
    }

    fn name(&mut self, global: u32, name: String) {
        if let Some(entry) = self.entries.get_mut(&global) {
            entry.pending_name = (!name.is_empty()).then_some(name);
        }
    }

    fn done(&mut self, global: u32) -> bool {
        let Some(entry) = self.entries.get_mut(&global) else {
            return false;
        };
        if entry.ready_name == entry.pending_name {
            return false;
        }
        entry.ready_name = entry.pending_name.clone();
        true
    }

    fn get(&self, name: &str) -> Option<&P> {
        let mut matching = self
            .entries
            .values()
            .filter(|entry| entry.ready_name.as_deref() == Some(name));
        let proxy = &matching.next()?.proxy;
        // A malformed duplicate name is ambiguous, not permission to bind an arbitrary output.
        matching.next().is_none().then_some(proxy)
    }

    fn remove(&mut self, global: u32) -> Option<P> {
        self.entries.remove(&global).map(|entry| entry.proxy)
    }

    fn clear(&mut self) -> Vec<P> {
        std::mem::take(&mut self.entries)
            .into_values()
            .map(|entry| entry.proxy)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_and_done_gate_readiness() {
        let mut cache = OutputCache::default();
        cache.insert(1, 10);
        assert_eq!(cache.get("DP-1"), None);
        assert!(!cache.done(1));
        cache.name(1, "DP-1".into());
        assert_eq!(cache.get("DP-1"), None);
        assert!(cache.done(1));
        assert_eq!(cache.get("DP-1"), Some(&10));
        assert!(!cache.done(1));
        assert_eq!(cache.get("missing"), None);
    }

    #[test]
    fn removed_global_cannot_be_resurrected_by_queued_events() {
        let mut cache = OutputCache::default();
        cache.insert(1, 10);
        cache.name(1, "DP-1".into());
        cache.done(1);
        assert_eq!(cache.remove(1), Some(10));
        assert_eq!(cache.get("DP-1"), None);
        cache.name(1, "DP-1".into());
        assert!(!cache.done(1));
        assert_eq!(cache.get("DP-1"), None);
        // Re-advertisement is a fresh proxy and must pass readiness again.
        cache.insert(2, 20);
        cache.name(2, "DP-1".into());
        assert_eq!(cache.get("DP-1"), None);
        cache.done(2);
        assert_eq!(cache.get("DP-1"), Some(&20));
    }

    #[test]
    fn empty_or_ambiguous_names_do_not_bind() {
        let mut cache = OutputCache::default();
        cache.insert(1, 10);
        cache.name(1, String::new());
        assert!(!cache.done(1));
        assert_eq!(cache.get(""), None);
        cache.name(1, "DP-1".into());
        cache.done(1);
        cache.insert(2, 20);
        cache.name(2, "DP-1".into());
        cache.done(2);
        assert_eq!(cache.get("DP-1"), None);
        cache.remove(1);
        assert_eq!(cache.get("DP-1"), Some(&20));
    }

    #[test]
    fn failure_invalidation_removes_all_ready_and_pending_proxies() {
        let mut cache = OutputCache::default();
        cache.insert(1, 10);
        cache.name(1, "DP-1".into());
        cache.done(1);
        cache.insert(2, 20);
        assert_eq!(cache.clear(), vec![10, 20]);
        assert_eq!(cache.get("DP-1"), None);
        assert!(!cache.done(2));
    }
}
