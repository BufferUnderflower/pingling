#![cfg_attr(libbox_stub, allow(dead_code))]

use domain::{ConnectionState, CoreEvent};
use pingle_netwatch::{IfaceSnapshot, UpdateEvent, Watcher};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const WATCH_POLL_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Debug)]
pub struct TunnelWatchHandle {
    stop_tx: mpsc::Sender<()>,
    join_handle: Option<JoinHandle<()>>,
}

impl TunnelWatchHandle {
    pub fn stop(mut self) {
        let _ = self.stop_tx.send(());
        if let Some(handle) = self.join_handle.take() {
            let _ = handle.join();
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TrackedTunnel {
    index: u32,
    name: String,
}

#[derive(Debug, Clone)]
struct TunnelMatcher {
    needles: Vec<String>,
}

impl TunnelMatcher {
    fn new(names: Vec<String>) -> Self {
        Self {
            needles: names
                .into_iter()
                .map(|name| name.trim().to_ascii_lowercase())
                .filter(|name| !name.is_empty())
                .collect(),
        }
    }

    fn matches_name(&self, name: &str) -> bool {
        let normalized = name.trim().to_ascii_lowercase();
        self.needles
            .iter()
            .any(|needle| normalized == *needle || normalized.contains(needle))
    }

    fn matches_snapshot(&self, snapshot: &IfaceSnapshot) -> bool {
        self.matches_name(&snapshot.name)
    }
}

pub fn default_tunnel_name_hints() -> Vec<String> {
    vec!["pingle-tun".into(), "sing-tun".into(), "tun0".into()]
}

pub fn start_tunnel_watch(
    watcher: Arc<dyn Watcher>,
    state: Arc<Mutex<ConnectionState>>,
    event_tx: Arc<Mutex<mpsc::Sender<CoreEvent>>>,
    tunnel_name_hints: Vec<String>,
) -> Result<TunnelWatchHandle, String> {
    let matcher = TunnelMatcher::new(tunnel_name_hints);
    let mut tracked = watcher
        .list_interfaces()?
        .into_values()
        .find(|iface| matcher.matches_snapshot(iface))
        .map(|iface| TrackedTunnel {
            index: iface.index,
            name: iface.name,
        });

    if tracked.is_some() {
        emit_state_if_changed(&state, &event_tx, ConnectionState::Connected);
        emit_log(&event_tx, "netwatch: tunnel interface already present before connect");
    }

    let events = watcher.subscribe()?;
    let (stop_tx, stop_rx) = mpsc::channel();
    let thread_state = state;
    let thread_event_tx = event_tx;

    let join_handle = thread::Builder::new()
        .name("pingle-libbox-windows-netwatch".into())
        .spawn(move || loop {
            if stop_rx.try_recv().is_ok() {
                break;
            }

            match events.recv_timeout(WATCH_POLL_INTERVAL) {
                Ok(event) => {
                    if let Some(next_state) = apply_update_event(&matcher, &mut tracked, &event) {
                        emit_state_if_changed(&thread_state, &thread_event_tx, next_state);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    emit_log(&thread_event_tx, "netwatch: subscription dropped");
                    break;
                }
            }
        })
        .map_err(|error| format!("spawn tunnel watcher thread: {error}"))?;

    Ok(TunnelWatchHandle {
        stop_tx,
        join_handle: Some(join_handle),
    })
}

fn apply_update_event(
    matcher: &TunnelMatcher,
    tracked: &mut Option<TrackedTunnel>,
    event: &UpdateEvent,
) -> Option<ConnectionState> {
    match event {
        UpdateEvent::Added(snapshot) | UpdateEvent::Modified { snapshot, .. }
            if matcher.matches_snapshot(snapshot) =>
        {
            *tracked = Some(TrackedTunnel {
                index: snapshot.index,
                name: snapshot.name.clone(),
            });
            Some(ConnectionState::Connected)
        }
        UpdateEvent::Removed { index, name }
            if matcher.matches_name(name)
                || tracked
                    .as_ref()
                    .map(|tunnel| tunnel.index == *index)
                    .unwrap_or(false) =>
        {
            *tracked = None;
            Some(ConnectionState::Disconnected)
        }
        _ => None,
    }
}

fn emit_state_if_changed(
    state: &Arc<Mutex<ConnectionState>>,
    event_tx: &Arc<Mutex<mpsc::Sender<CoreEvent>>>,
    next: ConnectionState,
) {
    let mut guard = state.lock().unwrap_or_else(|error| error.into_inner());
    if *guard == next {
        return;
    }
    *guard = next.clone();
    let _ = event_tx
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .send(CoreEvent::StateChanged(next));
}

fn emit_log(event_tx: &Arc<Mutex<mpsc::Sender<CoreEvent>>>, message: &str) {
    let _ = event_tx
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .send(CoreEvent::Log(message.to_string()));
}

#[cfg(test)]
mod tests {
    use super::{apply_update_event, default_tunnel_name_hints, start_tunnel_watch, TunnelMatcher};
    use domain::{ConnectionState, CoreEvent};
    use pingle_netwatch::{IfaceMap, IfaceSnapshot, UpdateEvent, Watcher};
    use std::collections::HashMap;
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    struct FakeWatcher {
        list: IfaceMap,
        subscribe_rx: Mutex<Option<mpsc::Receiver<UpdateEvent>>>,
    }

    impl FakeWatcher {
        fn new(
            list: IfaceMap,
            subscribe_rx: mpsc::Receiver<UpdateEvent>,
        ) -> Self {
            Self {
                list,
                subscribe_rx: Mutex::new(Some(subscribe_rx)),
            }
        }
    }

    impl Watcher for FakeWatcher {
        fn list_interfaces(&self) -> Result<IfaceMap, String> {
            Ok(self.list.clone())
        }

        fn subscribe(&self) -> Result<mpsc::Receiver<UpdateEvent>, String> {
            self.subscribe_rx
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take()
                .ok_or_else(|| "already subscribed".into())
        }
    }

    #[test]
    fn matcher_accepts_default_windows_names() {
        let matcher = TunnelMatcher::new(default_tunnel_name_hints());
        assert!(matcher.matches_name("pingle-tun"));
        assert!(matcher.matches_name("sing-tun tunnel"));
    }

    #[test]
    fn added_and_removed_events_toggle_connected_state() {
        let matcher = TunnelMatcher::new(default_tunnel_name_hints());
        let mut tracked = None;

        let added = UpdateEvent::Added(IfaceSnapshot {
            index: 11,
            name: "pingle-tun".into(),
            hw_addr: String::new(),
            ips: vec![],
        });
        let removed = UpdateEvent::Removed {
            index: 11,
            name: "if11".into(),
        };

        assert_eq!(
            apply_update_event(&matcher, &mut tracked, &added),
            Some(ConnectionState::Connected)
        );
        assert_eq!(
            apply_update_event(&matcher, &mut tracked, &removed),
            Some(ConnectionState::Disconnected)
        );
    }

    #[test]
    fn watch_thread_emits_connected_from_matching_update() {
        let (event_tx, event_rx) = mpsc::channel();
        let (watch_tx, watch_rx) = mpsc::channel();
        let watcher = Arc::new(FakeWatcher::new(HashMap::new(), watch_rx));
        let state = Arc::new(Mutex::new(ConnectionState::Connecting));
        let handle = start_tunnel_watch(
            watcher,
            state.clone(),
            Arc::new(Mutex::new(event_tx)),
            default_tunnel_name_hints(),
        )
        .expect("watch starts");

        watch_tx
            .send(UpdateEvent::Added(IfaceSnapshot {
                index: 7,
                name: "pingle-tun".into(),
                hw_addr: String::new(),
                ips: vec![],
            }))
            .unwrap();

        let event = event_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(event, CoreEvent::StateChanged(ConnectionState::Connected));
        assert_eq!(
            *state.lock().unwrap_or_else(|error| error.into_inner()),
            ConnectionState::Connected
        );

        handle.stop();
    }
}
