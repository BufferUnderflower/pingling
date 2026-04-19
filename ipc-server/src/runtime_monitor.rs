use crate::broadcaster::EventBroadcaster;
use crate::protocol_constants::events;
use core_clash_api::{ClashApiClient, ConnectionsSnapshot, TrafficSnapshot};
use serde_json::{json, Value};
use service::{RuntimeMetricsSnapshot, VpnManager};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

const MONITOR_POLL_INTERVAL: Duration = Duration::from_millis(500);
const RECONNECT_DELAY: Duration = Duration::from_secs(1);

pub fn spawn_runtime_monitor(vpn: Arc<VpnManager>, broadcaster: Arc<EventBroadcaster>) {
    thread::Builder::new()
        .name("pingle-runtime-monitor".into())
        .spawn(move || {
            let generation = Arc::new(AtomicU64::new(0));
            let mut active_controller: Option<String> = None;
            loop {
                if !vpn.is_running() {
                    active_controller = reset_monitor_state(
                        &vpn,
                        &broadcaster,
                        &generation,
                        active_controller.take(),
                    );
                    thread::sleep(MONITOR_POLL_INTERVAL);
                    continue;
                }

                let controller = match vpn.current_clash_controller() {
                    Ok(Some(controller)) => controller,
                    Ok(None) => {
                        active_controller = reset_monitor_state(
                            &vpn,
                            &broadcaster,
                            &generation,
                            active_controller.take(),
                        );
                        thread::sleep(MONITOR_POLL_INTERVAL);
                        continue;
                    }
                    Err(error) => {
                        log::debug!("runtime monitor: resolve controller failed: {error}");
                        thread::sleep(MONITOR_POLL_INTERVAL);
                        continue;
                    }
                };

                if active_controller.as_deref() != Some(controller.as_str()) {
                    let next_generation = generation.fetch_add(1, Ordering::SeqCst) + 1;
                    active_controller = Some(controller.clone());
                    initialize_snapshot(&vpn, &broadcaster, &controller);
                    spawn_traffic_reader(
                        vpn.clone(),
                        broadcaster.clone(),
                        generation.clone(),
                        next_generation,
                        controller.clone(),
                    );
                    spawn_connections_reader(
                        vpn.clone(),
                        broadcaster.clone(),
                        generation.clone(),
                        next_generation,
                        controller,
                    );
                }

                thread::sleep(MONITOR_POLL_INTERVAL);
            }
        })
        .ok();
}

fn initialize_snapshot(
    vpn: &Arc<VpnManager>,
    broadcaster: &Arc<EventBroadcaster>,
    controller: &str,
) {
    let clash_version = ClashApiClient::new(controller)
        .and_then(|client| client.get_version())
        .map(|version| version.version)
        .ok();

    apply_metrics_update(vpn, broadcaster, |snapshot| {
        snapshot.available = true;
        snapshot.controller = Some(controller.to_string());
        snapshot.clash_version = clash_version;
    });
}

fn spawn_traffic_reader(
    vpn: Arc<VpnManager>,
    broadcaster: Arc<EventBroadcaster>,
    generation: Arc<AtomicU64>,
    expected_generation: u64,
    controller: String,
) {
    thread::Builder::new()
        .name("pingle-runtime-traffic".into())
        .spawn(move || loop {
            if generation.load(Ordering::SeqCst) != expected_generation {
                return;
            }
            let client = match ClashApiClient::new(&controller) {
                Ok(client) => client,
                Err(error) => {
                    log::debug!("runtime monitor: traffic client failed: {error}");
                    thread::sleep(RECONNECT_DELAY);
                    continue;
                }
            };
            let mut stream = match client.subscribe_traffic() {
                Ok(stream) => stream,
                Err(error) => {
                    log::debug!("runtime monitor: traffic subscribe failed: {error}");
                    thread::sleep(RECONNECT_DELAY);
                    continue;
                }
            };
            loop {
                if generation.load(Ordering::SeqCst) != expected_generation {
                    return;
                }
                match stream.recv() {
                    Ok(snapshot) => {
                        update_traffic_metrics(&vpn, &broadcaster, &controller, snapshot)
                    }
                    Err(error) => {
                        log::debug!("runtime monitor: traffic stream closed: {error}");
                        break;
                    }
                }
            }
            thread::sleep(RECONNECT_DELAY);
        })
        .ok();
}

fn spawn_connections_reader(
    vpn: Arc<VpnManager>,
    broadcaster: Arc<EventBroadcaster>,
    generation: Arc<AtomicU64>,
    expected_generation: u64,
    controller: String,
) {
    thread::Builder::new()
        .name("pingle-runtime-connections".into())
        .spawn(move || loop {
            if generation.load(Ordering::SeqCst) != expected_generation {
                return;
            }
            let client = match ClashApiClient::new(&controller) {
                Ok(client) => client,
                Err(error) => {
                    log::debug!("runtime monitor: connections client failed: {error}");
                    thread::sleep(RECONNECT_DELAY);
                    continue;
                }
            };
            let mut stream = match client.subscribe_connections() {
                Ok(stream) => stream,
                Err(error) => {
                    log::debug!("runtime monitor: connections subscribe failed: {error}");
                    thread::sleep(RECONNECT_DELAY);
                    continue;
                }
            };
            loop {
                if generation.load(Ordering::SeqCst) != expected_generation {
                    return;
                }
                match stream.recv() {
                    Ok(snapshot) => {
                        update_connection_metrics(&vpn, &broadcaster, &controller, snapshot)
                    }
                    Err(error) => {
                        log::debug!("runtime monitor: connections stream closed: {error}");
                        break;
                    }
                }
            }
            thread::sleep(RECONNECT_DELAY);
        })
        .ok();
}

fn update_traffic_metrics(
    vpn: &Arc<VpnManager>,
    broadcaster: &Arc<EventBroadcaster>,
    controller: &str,
    snapshot: TrafficSnapshot,
) {
    apply_metrics_update(vpn, broadcaster, |current| {
        current.available = true;
        current.controller = Some(controller.to_string());
        current.upload_bps = Some(snapshot.up);
        current.download_bps = Some(snapshot.down);
    });
}

fn update_connection_metrics(
    vpn: &Arc<VpnManager>,
    broadcaster: &Arc<EventBroadcaster>,
    controller: &str,
    snapshot: ConnectionsSnapshot,
) {
    apply_metrics_update(vpn, broadcaster, |current| {
        current.available = true;
        current.controller = Some(controller.to_string());
        current.upload_total = Some(snapshot.upload_total);
        current.download_total = Some(snapshot.download_total);
        current.connections_count = Some(snapshot.connections.len());
        current.memory_bytes = Some(snapshot.memory);
    });
}

fn reset_monitor_state(
    vpn: &Arc<VpnManager>,
    broadcaster: &Arc<EventBroadcaster>,
    generation: &Arc<AtomicU64>,
    active_controller: Option<String>,
) -> Option<String> {
    if active_controller.is_some() {
        generation.fetch_add(1, Ordering::SeqCst);
    }
    if vpn.runtime_metrics() != RuntimeMetricsSnapshot::default() {
        vpn.clear_runtime_metrics();
        broadcaster.publish_custom(
            events::RUNTIME_METRICS_CHANGED,
            metrics_to_json(&vpn.runtime_metrics()),
        );
    }
    None
}

fn apply_metrics_update<F>(vpn: &Arc<VpnManager>, broadcaster: &Arc<EventBroadcaster>, mutate: F)
where
    F: FnOnce(&mut RuntimeMetricsSnapshot),
{
    let mut next = vpn.runtime_metrics();
    let previous = next.clone();
    mutate(&mut next);
    if next != previous {
        vpn.set_runtime_metrics(next.clone());
        broadcaster.publish_custom(events::RUNTIME_METRICS_CHANGED, metrics_to_json(&next));
    }
}

pub fn metrics_to_json(snapshot: &RuntimeMetricsSnapshot) -> Value {
    json!({
        "available": snapshot.available,
        "controller": snapshot.controller,
        "clash_version": snapshot.clash_version,
        "upload_bps": snapshot.upload_bps,
        "download_bps": snapshot.download_bps,
        "upload_total": snapshot.upload_total,
        "download_total": snapshot.download_total,
        "connections_count": snapshot.connections_count,
        "memory_bytes": snapshot.memory_bytes,
    })
}
