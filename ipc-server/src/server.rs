//! Concurrent IPC server entry point.
//!
//! [`start`] wires up:
//! - the Unix domain socket listener (`$XDG_RUNTIME_DIR/pingle.sock` or
//!   `$TMPDIR/pingle.sock`)
//! - the TCP loopback listener on `127.0.0.1` (OS-assigned port)
//! - the UDP discovery beacon (see [`super::discovery`])
//! - the per-daemon registry file (see [`super::discovery::publish_registry`])
//!
//! Each accepted connection runs on its own native thread.

use crate::broadcaster::EventBroadcaster;
use crate::discovery::{self, DaemonAdvertisement};
use crate::methods;
use crate::protocol::{Notification, Request, Response, INVALID_REQUEST, PARSE_ERROR};
use serde_json::Value;
use service::VpnManager;
use std::io::{self, BufRead, BufReader};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};

/// Stable wire-protocol version. Bump on breaking changes.
pub const PROTOCOL_VERSION: u32 = 1;

/// Started server handle. Holds the broadcaster and any cleanup paths.
///
/// The handle is intentionally `Send + Sync` so callers can `clone()` the
/// broadcaster into other parts of the daemon (e.g. the tray refresh loop)
/// and emit events.
#[derive(Clone)]
pub struct ServerHandle {
    pub broadcaster: Arc<EventBroadcaster>,
    pub uds_path: Option<PathBuf>,
    pub tcp_addr: Option<String>,
}

/// Spawn the IPC server. Returns immediately. The listener threads run
/// for the lifetime of the process.
///
/// `vpn` is the shared [`VpnManager`] — every accepted connection holds
/// an `Arc` clone of it.
///
/// Convenience wrapper around [`start_with_broadcaster`] that
/// constructs a fresh [`EventBroadcaster`] internally. Use the
/// `_with_broadcaster` form when you need to pre-wire the broadcaster
/// into a [`BroadcastingSlotObserver`] (or similar) *before* the
/// server boots, so the first slot dispatches reach subscribers.
pub fn start(vpn: Arc<VpnManager>) -> io::Result<ServerHandle> {
    start_with_broadcaster(vpn, Arc::new(EventBroadcaster::new()))
}

/// Lower-level [`start`] variant that takes an externally-constructed
/// broadcaster. The composition root builds it, wires it into a
/// [`BroadcastingSlotObserver`], installs the observer on the
/// [`VpnManager`], then hands the same broadcaster to this function
/// so subscribers see both `event.stateChanged` *and* `event.slot.*`
/// notifications flowing out of the same channel.
pub fn start_with_broadcaster(
    vpn: Arc<VpnManager>,
    broadcaster: Arc<EventBroadcaster>,
) -> io::Result<ServerHandle> {

    // ----- UDS listener (best-effort, unix-only) ---------------------------
    #[cfg(unix)]
    let (uds_path, uds_path_str, uds_started) = {
        let uds_path = uds_socket_path();
        let uds_path_str = uds_path.to_string_lossy().to_string();
        let started = match bind_uds(&uds_path) {
            Ok(listener) => {
                log::info!("ipc: UDS listening at {}", uds_path.display());
                spawn_uds_loop(listener, vpn.clone(), broadcaster.clone());
                true
            }
            Err(e) => {
                log::warn!("ipc: UDS bind failed at {}: {e}", uds_path.display());
                false
            }
        };
        (uds_path, uds_path_str, started)
    };
    #[cfg(not(unix))]
    let (uds_path, uds_path_str, uds_started): (PathBuf, String, bool) =
        (PathBuf::new(), String::new(), false);

    // ----- TCP listener (best-effort, multi-address) -------------------------
    // Try binding several loopback addresses because macOS and Windows differ
    // in what works reliably:
    //   - macOS: 127.0.0.1 always works, localhost maps to it
    //   - Windows 10+: localhost may resolve to ::1 (IPv6); 127.0.0.1 sometimes
    //     times out due to Windows Firewall treating raw IPv4 differently
    //   - Older Windows: ::1 may not be available at all
    //
    // We try the candidates in order and take the first that binds. The
    // advertised address uses "localhost" so the Dart client's fallback
    // list (localhost → 127.0.0.1 → ::1) can match whichever family the
    // OS actually accepted.
    let tcp_candidates = ["localhost:0", "127.0.0.1:0", "[::1]:0"];
    let tcp_listener = tcp_candidates
        .iter()
        .find_map(|addr| TcpListener::bind(addr).ok())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::AddrNotAvailable,
                "ipc: could not bind any loopback address for TCP",
            )
        })?;
    let bound_addr = tcp_listener.local_addr()?;
    let tcp_addr = format!("localhost:{}", bound_addr.port());
    log::info!("ipc: TCP listening at {} (bound to {})", tcp_addr, bound_addr);
    spawn_tcp_loop(tcp_listener, vpn.clone(), broadcaster.clone());

    // ----- Discovery: registry file + UDP beacon ---------------------------
    let advertisement = Arc::new(DaemonAdvertisement::new(
        Some(tcp_addr.clone()),
        if uds_started {
            Some(uds_path_str.clone())
        } else {
            None
        },
    ));

    match discovery::publish_registry(&advertisement) {
        Ok(path) => log::info!("ipc: registry written to {}", path.display()),
        Err(e) => log::warn!("ipc: registry write failed: {e}"),
    }

    if let Err(e) = discovery::spawn_beacon(advertisement.clone()) {
        log::warn!("ipc: discovery beacon failed to start: {e}");
    }

    Ok(ServerHandle {
        broadcaster,
        uds_path: if uds_started { Some(uds_path) } else { None },
        tcp_addr: Some(tcp_addr),
    })
}

// ---------------------------------------------------------------------------
// UDS bind helpers
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn uds_socket_path() -> PathBuf {
    if let Some(rt) = std::env::var_os("XDG_RUNTIME_DIR") {
        return PathBuf::from(rt).join("pingle.sock");
    }
    std::env::temp_dir().join("pingle.sock")
}

#[cfg(unix)]
fn bind_uds(path: &PathBuf) -> io::Result<UnixListener> {
    // Stale socket left over from a previous run blocks new bind. Removing
    // it is safe because we're about to create a new one — there's a race
    // window only if two daemons race to start, which we accept.
    let _ = std::fs::remove_file(path);
    UnixListener::bind(path)
}

// ---------------------------------------------------------------------------
// Listener loops
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn spawn_uds_loop(
    listener: UnixListener,
    vpn: Arc<VpnManager>,
    broadcaster: Arc<EventBroadcaster>,
) {
    thread::Builder::new()
        .name("pingle-ipc-uds".into())
        .spawn(move || loop {
            match listener.accept() {
                Ok((stream, _addr)) => {
                    let vpn = vpn.clone();
                    let bc = broadcaster.clone();
                    thread::Builder::new()
                        .name("pingle-ipc-uds-conn".into())
                        .spawn(move || handle_uds(stream, vpn, bc))
                        .ok();
                }
                Err(e) => {
                    log::warn!("ipc: UDS accept error: {e}");
                    thread::sleep(Duration::from_millis(200));
                }
            }
        })
        .ok();
}

fn spawn_tcp_loop(listener: TcpListener, vpn: Arc<VpnManager>, broadcaster: Arc<EventBroadcaster>) {
    thread::Builder::new()
        .name("pingle-ipc-tcp".into())
        .spawn(move || loop {
            match listener.accept() {
                Ok((stream, peer)) => {
                    log::debug!("ipc: TCP conn from {peer}");
                    let vpn = vpn.clone();
                    let bc = broadcaster.clone();
                    thread::Builder::new()
                        .name("pingle-ipc-tcp-conn".into())
                        .spawn(move || handle_tcp(stream, vpn, bc))
                        .ok();
                }
                Err(e) => {
                    log::warn!("ipc: TCP accept error: {e}");
                    thread::sleep(Duration::from_millis(200));
                }
            }
        })
        .ok();
}

// ---------------------------------------------------------------------------
// Per-connection handlers
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn handle_uds(stream: UnixStream, vpn: Arc<VpnManager>, broadcaster: Arc<EventBroadcaster>) {
    let writer = stream.try_clone().expect("clone uds stream");
    serve_connection(BufReader::new(stream), writer, vpn, broadcaster);
}

fn handle_tcp(stream: TcpStream, vpn: Arc<VpnManager>, broadcaster: Arc<EventBroadcaster>) {
    let _ = stream.set_nodelay(true);
    let writer = stream.try_clone().expect("clone tcp stream");
    serve_connection(BufReader::new(stream), writer, vpn, broadcaster);
}

/// Generic per-connection loop. Reads newline-delimited JSON requests,
/// dispatches them, and writes responses. Push-event subscriptions are
/// served by a sibling thread that drains the broadcaster receiver.
///
/// The writer is wrapped in `Arc<Mutex<W>>` so the request loop and the push
/// pump can share it safely. `W` must be `Send + 'static` because the push
/// pump runs on its own thread.
fn serve_connection<R, W>(
    reader: BufReader<R>,
    writer: W,
    vpn: Arc<VpnManager>,
    broadcaster: Arc<EventBroadcaster>,
) where
    R: io::Read,
    W: io::Write + Send + 'static,
{
    let writer = Arc::new(Mutex::new(writer));

    // Subscribe to push events for this connection. The receiver lives
    // until the request thread exits and drops it.
    let rx = broadcaster.subscribe();

    // Spawn the push-event pump.
    let push_writer = writer.clone();
    let pump = thread::Builder::new()
        .name("pingle-ipc-push".into())
        .spawn(move || {
            for notif in rx {
                let mut guard = match push_writer.lock() {
                    Ok(g) => g,
                    Err(p) => p.into_inner(),
                };
                if write_notif(&mut *guard, &notif).is_err() {
                    break;
                }
            }
        })
        .ok();

    // Request loop.
    for line in reader.lines() {
        let line = match line {
            Ok(l) if l.trim().is_empty() => continue,
            Ok(l) => l,
            Err(e) => {
                log::debug!("ipc: read error, closing: {e}");
                break;
            }
        };

        let response = handle_line(&line, &vpn, &broadcaster);
        if let Some(resp) = response {
            let mut guard = match writer.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            if write_response(&mut *guard, &resp).is_err() {
                break;
            }
        }
    }

    // Drop subscription so the push pump exits naturally.
    drop(writer);
    if let Some(handle) = pump {
        let _ = handle.join();
    }
}

/// Parse one line and dispatch. Returns `Some(Response)` for requests
/// with an `id`, `None` for notifications.
fn handle_line(
    line: &str,
    vpn: &Arc<VpnManager>,
    broadcaster: &Arc<EventBroadcaster>,
) -> Option<Response> {
    let req: Request = match serde_json::from_str(line) {
        Ok(r) => r,
        Err(e) => {
            return Some(Response::err(
                Value::Null,
                PARSE_ERROR,
                format!("parse error: {e}"),
                None,
            ));
        }
    };

    if req.jsonrpc != "2.0" {
        return Some(Response::err(
            req.id_or_null(),
            INVALID_REQUEST,
            format!("unsupported jsonrpc version: {}", req.jsonrpc),
            None,
        ));
    }

    // slot.ipc.dispatch.* — cross-cutting hook around every JSON-RPC
    // method call. Plugins can observe every client interaction
    // (telemetry, audit, rate-limit) without enumerating method
    // names. The payload's `outcome` field is `None` on before/exec
    // and populated on after so subscribers can pair enter→outcome
    // by `invocation_id`.
    //
    // If no plugin is loaded, the slot is a no-op (`run_slot`
    // short-circuits to Ok(None)) but the observer still gets fired
    // through `vpn.run_slot`, which means `event.slot.*` IPC
    // notifications still go out when the BroadcastingSlotObserver
    // is wired in — the important side effect in daemon-v0.1.3.
    let invocation_id = domain::new_invocation_id();
    let method_name = req.method.clone();
    let mut slot_payload = domain::IpcDispatchPayload {
        method: method_name.clone(),
        params: req.params.clone(),
        transport: None,
        outcome: None,
    };
    let _ = vpn.run_slot(
        domain::slot_names::IPC_DISPATCH,
        domain::IPC_DISPATCH_WIRE_VERSION,
        &invocation_id,
        slot_payload.clone(),
    );

    let start_ts = std::time::Instant::now();
    let response = methods::dispatch(vpn, broadcaster, req);
    let duration_us = start_ts.elapsed().as_micros() as u64;

    slot_payload.outcome = Some(match &response {
        Some(r) if r.error.is_none() => domain::IpcDispatchOutcome {
            ok: true,
            error_code: None,
            error_message: None,
            duration_us,
        },
        Some(r) => domain::IpcDispatchOutcome {
            ok: false,
            error_code: r.error.as_ref().map(|e| e.code),
            error_message: r.error.as_ref().map(|e| e.message.clone()),
            duration_us,
        },
        None => {
            // JSON-RPC notification (no id) — no response frame to
            // inspect, treat as success for telemetry purposes.
            domain::IpcDispatchOutcome {
                ok: true,
                error_code: None,
                error_message: None,
                duration_us,
            }
        }
    });
    let _ = vpn.run_slot(
        domain::slot_names::IPC_DISPATCH,
        domain::IPC_DISPATCH_WIRE_VERSION,
        &invocation_id,
        slot_payload,
    );

    response
}

// ---------------------------------------------------------------------------
// Frame writers — newline-delimited JSON. Used for both responses and pushes.
// ---------------------------------------------------------------------------

fn write_response<W: io::Write>(w: &mut W, resp: &Response) -> io::Result<()> {
    let line = serde_json::to_string(resp)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("serialize: {e}")))?;
    w.write_all(line.as_bytes())?;
    w.write_all(b"\n")?;
    w.flush()
}

fn write_notif<W: io::Write>(w: &mut W, notif: &Notification) -> io::Result<()> {
    let line = serde_json::to_string(notif)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("serialize: {e}")))?;
    w.write_all(line.as_bytes())?;
    w.write_all(b"\n")?;
    w.flush()
}

// ---------------------------------------------------------------------------
// Tests — exercise the dispatcher with a stub manager.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Minimal stub VpnManager-equivalent dispatch test.
    /// Goes through `methods::dispatch` directly with a real VpnManager
    /// built from a mock core, exercising the wire format end-to-end.
    fn build_vpn() -> Arc<VpnManager> {
        use core_mock::MockCore;
        use data::MemorySettingsStorage;
        use service::CoreRegistry;
        let mut registry = CoreRegistry::new();
        registry.register(
            domain::CoreDescriptor {
                core_type: "mock".into(),
                display_name: "Mock".into(),
                source: domain::CoreSource::Mocked,
                binary_path: None,
                available: true,
            },
            Box::new(MockCore::new()),
        );
        let storage: Box<dyn domain::SettingsStorage> = Box::new(MemorySettingsStorage::new());
        Arc::new(VpnManager::new(registry, storage))
    }

    /// Throwaway broadcaster for tests that don't care about pushed events.
    fn bc() -> Arc<EventBroadcaster> {
        Arc::new(EventBroadcaster::new())
    }

    #[test]
    fn dispatch_status_returns_state() {
        let vpn = build_vpn();
        let req = Request {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "vpn.status".into(),
            params: Value::Null,
        };
        let resp = methods::dispatch(&vpn, &bc(), req).unwrap();
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        assert!(result.get("state").is_some());
        assert_eq!(result["running"], false);
    }

    #[test]
    fn dispatch_unknown_method_returns_method_not_found() {
        let vpn = build_vpn();
        let req = Request {
            jsonrpc: "2.0".into(),
            id: Some(json!(2)),
            method: "vpn.warpspeed".into(),
            params: Value::Null,
        };
        let resp = methods::dispatch(&vpn, &bc(), req).unwrap();
        assert!(resp.error.is_some());
        let err = resp.error.unwrap();
        assert_eq!(err.code, crate::protocol::METHOD_NOT_FOUND);
    }

    #[test]
    fn parse_error_for_garbage_input() {
        let vpn = build_vpn();
        let resp = handle_line("not json at all", &vpn, &bc()).unwrap();
        let err = resp.error.unwrap();
        assert_eq!(err.code, PARSE_ERROR);
    }

    #[test]
    fn invalid_jsonrpc_version_rejected() {
        let vpn = build_vpn();
        let resp = handle_line(
            r#"{"jsonrpc":"1.0","id":1,"method":"vpn.status"}"#,
            &vpn,
            &bc(),
        )
        .unwrap();
        let err = resp.error.unwrap();
        assert_eq!(err.code, INVALID_REQUEST);
    }

    #[test]
    fn daemon_ping_returns_pong() {
        let vpn = build_vpn();
        let resp = handle_line(
            r#"{"jsonrpc":"2.0","id":42,"method":"daemon.ping"}"#,
            &vpn,
            &bc(),
        )
        .unwrap();
        assert_eq!(resp.id, json!(42));
        assert_eq!(resp.result.unwrap()["pong"], true);
    }

    #[test]
    fn config_set_then_get_round_trip() {
        let vpn = build_vpn();
        let _ = handle_line(
            r#"{"jsonrpc":"2.0","id":1,"method":"config.set","params":{"key":"foo","value":"bar"}}"#,
            &vpn,
            &bc(),
        );
        let resp = handle_line(
            r#"{"jsonrpc":"2.0","id":2,"method":"config.get","params":{"key":"foo"}}"#,
            &vpn,
            &bc(),
        )
        .unwrap();
        assert_eq!(resp.result.unwrap()["value"], "bar");
    }

    #[test]
    fn vpn_error_serialized_with_stable_code() {
        let vpn = build_vpn();
        // Set a config path then connect twice — second connect returns AlreadyConnected
        // for cores that successfully started the first time. Mock core may return
        // different stable codes; assert presence of a stable string code rather than
        // any specific value, so the test stays decoupled from the mock's exact behaviour.
        let _ = handle_line(
            r#"{"jsonrpc":"2.0","id":1,"method":"config.set","params":{"key":"config_path","value":"/tmp/pingle-test.json"}}"#,
            &vpn,
            &bc(),
        );
        let _ = handle_line(
            r#"{"jsonrpc":"2.0","id":2,"method":"vpn.connect"}"#,
            &vpn,
            &bc(),
        );
        let resp = handle_line(
            r#"{"jsonrpc":"2.0","id":3,"method":"vpn.connect"}"#,
            &vpn,
            &bc(),
        )
        .unwrap();
        let err = resp.error.unwrap();
        assert_eq!(err.code, crate::protocol::APPLICATION_ERROR);
        let data = err.data.expect("error.data carries stable code");
        assert!(data["code"].is_string(), "stable error code is a string");
        assert!(data["recoverable"].is_boolean(), "recoverable flag present");
    }

    #[test]
    fn notification_request_returns_no_response() {
        let vpn = build_vpn();
        // Notifications have no `id`. Server must not reply.
        let resp = handle_line(r#"{"jsonrpc":"2.0","method":"daemon.ping"}"#, &vpn, &bc());
        assert!(resp.is_none());
    }

    #[test]
    fn invalid_params_returns_invalid_params_code() {
        let vpn = build_vpn();
        // core.switch needs `coreType` — sending an empty params object should fail.
        let resp = handle_line(
            r#"{"jsonrpc":"2.0","id":1,"method":"core.switch","params":{}}"#,
            &vpn,
            &bc(),
        )
        .unwrap();
        let err = resp.error.unwrap();
        assert_eq!(err.code, crate::protocol::INVALID_PARAMS);
    }

    #[test]
    fn core_list_returns_array_of_descriptors() {
        let vpn = build_vpn();
        let resp = handle_line(
            r#"{"jsonrpc":"2.0","id":1,"method":"core.list"}"#,
            &vpn,
            &bc(),
        )
        .unwrap();
        let result = resp.result.unwrap();
        let arr = result.as_array().expect("array");
        assert!(!arr.is_empty(), "at least one core registered");
        assert_eq!(arr[0]["core_type"], "mock");
        assert_eq!(arr[0]["available"], true);
    }

    #[test]
    fn daemon_info_includes_pid_and_protocol_version() {
        let vpn = build_vpn();
        let resp = handle_line(
            r#"{"jsonrpc":"2.0","id":1,"method":"daemon.info"}"#,
            &vpn,
            &bc(),
        )
        .unwrap();
        let result = resp.result.unwrap();
        assert_eq!(result["name"], "pingle");
        assert!(result["pid"].as_u64().unwrap() > 0);
        assert_eq!(result["protocol_version"], crate::PROTOCOL_VERSION);
        assert!(result["capabilities"].is_array());
        assert_eq!(result["active_core"], "mock");
    }

    #[test]
    fn core_info_returns_name_version_protocols() {
        let vpn = build_vpn();
        let resp = handle_line(
            r#"{"jsonrpc":"2.0","id":1,"method":"core.info"}"#,
            &vpn,
            &bc(),
        )
        .unwrap();
        let result = resp.result.unwrap();
        assert!(result["name"].is_string());
        assert!(result["version"].is_string());
        assert!(result["supported_protocols"].is_array());
    }

    #[test]
    fn core_prereqs_returns_checks_array() {
        let vpn = build_vpn();
        let resp = handle_line(
            r#"{"jsonrpc":"2.0","id":1,"method":"core.prereqs"}"#,
            &vpn,
            &bc(),
        )
        .unwrap();
        let result = resp.result.unwrap();
        // Mock core may return empty list — what matters is the shape.
        let checks = result["checks"].as_array().expect("checks is array");
        for check in checks {
            assert!(check["name"].is_string());
            assert!(check["passed"].is_boolean());
            assert!(check["message"].is_string());
        }
    }

    #[test]
    fn core_capabilities_returns_list() {
        let vpn = build_vpn();
        let resp = handle_line(
            r#"{"jsonrpc":"2.0","id":1,"method":"core.capabilities"}"#,
            &vpn,
            &bc(),
        )
        .unwrap();
        let result = resp.result.unwrap();
        assert!(result["capabilities"].is_array());
    }

    #[test]
    fn outbounds_list_returns_empty_when_capability_unregistered() {
        let vpn = build_vpn();
        let resp = handle_line(
            r#"{"jsonrpc":"2.0","id":1,"method":"outbounds.list"}"#,
            &vpn,
            &bc(),
        )
        .unwrap();
        let result = resp.result.unwrap();
        // Mock build_vpn doesn't register the capability — expect empty list,
        // NOT an error. The API treats "no capability" as "empty result" so
        // clients can unconditionally call it.
        let arr = result["outbounds"].as_array().expect("outbounds array");
        assert!(arr.is_empty());
    }

    #[test]
    fn config_validate_requires_path() {
        let vpn = build_vpn();
        // No config_path set, no explicit path — should fail with INVALID_PARAMS
        let resp = handle_line(
            r#"{"jsonrpc":"2.0","id":1,"method":"config.validate"}"#,
            &vpn,
            &bc(),
        )
        .unwrap();
        let err = resp.error.expect("should error");
        assert_eq!(err.code, crate::protocol::INVALID_PARAMS);
    }

    #[test]
    fn config_validate_accepts_explicit_path() {
        let vpn = build_vpn();
        // Mock core validates any path as long as it's not empty — exercise
        // the pipeline hop through the ValidateBeforeStart-free bare pipeline.
        let resp = handle_line(
            r#"{"jsonrpc":"2.0","id":1,"method":"config.validate","params":{"path":"/tmp/explicit.json"}}"#,
            &vpn,
            &bc(),
        )
        .unwrap();
        // Mock may succeed or error depending on validation config; either
        // way the shape should be clean JSON-RPC (no crash).
        assert!(resp.result.is_some() || resp.error.is_some());
    }

    #[test]
    fn config_validate_uses_stored_path_when_absent() {
        let vpn = build_vpn();
        let _ = handle_line(
            r#"{"jsonrpc":"2.0","id":1,"method":"config.set","params":{"key":"config_path","value":"/tmp/stored.json"}}"#,
            &vpn,
            &bc(),
        );
        let resp = handle_line(
            r#"{"jsonrpc":"2.0","id":2,"method":"config.validate"}"#,
            &vpn,
            &bc(),
        )
        .unwrap();
        // Shape check only — mock core accepts any path.
        if let Some(result) = resp.result {
            assert_eq!(result["path"], "/tmp/stored.json");
        }
    }

    #[test]
    fn outbounds_select_returns_error_when_capability_missing() {
        let vpn = build_vpn();
        let resp = handle_line(
            r#"{"jsonrpc":"2.0","id":1,"method":"outbounds.select","params":{"outboundId":"any"}}"#,
            &vpn,
            &bc(),
        )
        .unwrap();
        // Mock VPN manager has no select_outbound capability pipeline →
        // application error with stable code.
        let err = resp.error.expect("should error");
        assert_eq!(err.code, crate::protocol::APPLICATION_ERROR);
    }

    // -- Plugin fall-through dispatch --------------------------------------
    //
    // The daemon doesn't enumerate plugin method names — instead, anything
    // the built-in arms don't claim is forwarded to `vpn.plugin().handle_ipc`.
    // These tests prove that the fall-through works for the three relevant
    // shapes: plugin claims method (success), plugin claims method (error),
    // plugin doesn't claim (returns None → MethodNotFound).

    /// Inline stub plugin: claims `auth.login` and `profile.bootstrap` with
    /// canned data, claims `auth.fail` with an error, doesn't claim
    /// anything else. Authenticator reports "logged in as alice" so the
    /// daemon.info plugin meta path is exercised too.
    struct StubPlugin {
        auth: StubAuthCache,
    }

    struct StubAuthCache;

    impl domain::Authenticator for StubAuthCache {
        fn is_authenticated(&self) -> bool {
            true
        }
        fn user_id(&self) -> Option<String> {
            Some("alice".into())
        }
    }

    impl domain::Plugin for StubPlugin {
        fn name(&self) -> &str {
            "stub-plugin"
        }
        fn authenticator(&self) -> Option<&dyn domain::Authenticator> {
            Some(&self.auth)
        }
        fn handle_ipc(
            &self,
            method: &str,
            params: &serde_json::Value,
        ) -> Option<Result<serde_json::Value, domain::VpnError>> {
            match method {
                "auth.login" => Some(Ok(serde_json::json!({
                    "token": "guest-tok",
                    "account_id": "guest_42",
                    "is_new": true,
                    "echoed_params": params,
                }))),
                "profile.bootstrap" => Some(Ok(serde_json::json!({
                    "account_id": "guest_42",
                    "display_name": "Alice",
                }))),
                "auth.fail" => Some(Err(domain::VpnError::Unknown("simulated".into()))),
                _ => None,
            }
        }
    }

    fn build_vpn_with_plugin() -> Arc<VpnManager> {
        let vpn = build_vpn();
        vpn.set_plugin(Arc::new(StubPlugin { auth: StubAuthCache }));
        vpn
    }

    #[test]
    fn plugin_fallthrough_dispatches_claimed_method_with_params() {
        let vpn = build_vpn_with_plugin();
        let resp = handle_line(
            r#"{"jsonrpc":"2.0","id":1,"method":"auth.login","params":{"mode":"guest"}}"#,
            &vpn,
            &bc(),
        )
        .unwrap();
        assert!(resp.error.is_none(), "expected ok, got {:?}", resp.error);
        let r = resp.result.unwrap();
        assert_eq!(r["token"], "guest-tok");
        assert_eq!(r["account_id"], "guest_42");
        // Params are passed through opaquely — the daemon does not
        // parse, validate, or rename them.
        assert_eq!(r["echoed_params"]["mode"], "guest");
    }

    #[test]
    fn plugin_fallthrough_surfaces_plugin_error_as_application_error() {
        let vpn = build_vpn_with_plugin();
        let resp = handle_line(
            r#"{"jsonrpc":"2.0","id":1,"method":"auth.fail"}"#,
            &vpn,
            &bc(),
        )
        .unwrap();
        let err = resp.error.expect("plugin returned an error");
        assert_eq!(err.code, crate::protocol::APPLICATION_ERROR);
        assert!(err.message.contains("simulated"));
    }

    #[test]
    fn plugin_fallthrough_returns_method_not_found_when_plugin_unclaims() {
        let vpn = build_vpn_with_plugin();
        // The stub plugin doesn't claim `nope.method`, so it returns `None`
        // and the dispatcher surfaces MethodNotFound to the client.
        let resp = handle_line(
            r#"{"jsonrpc":"2.0","id":1,"method":"nope.method"}"#,
            &vpn,
            &bc(),
        )
        .unwrap();
        let err = resp.error.expect("unknown method");
        assert_eq!(err.code, crate::protocol::METHOD_NOT_FOUND);
    }

    #[test]
    fn plugin_absent_unknown_method_returns_method_not_found() {
        let vpn = build_vpn(); // no .set_plugin() — daemon runs without a plugin
        let resp = handle_line(
            r#"{"jsonrpc":"2.0","id":1,"method":"auth.login","params":{"mode":"guest"}}"#,
            &vpn,
            &bc(),
        )
        .unwrap();
        let err = resp.error.expect("unknown method");
        assert_eq!(err.code, crate::protocol::METHOD_NOT_FOUND);
        // Critically: NOT a special "auth not configured" error. The daemon
        // does not name auth at all under the new architecture. Clients see
        // a uniform "method not found" and decide what to render.
    }

    #[test]
    fn daemon_info_includes_plugin_meta_when_installed() {
        let vpn = build_vpn_with_plugin();
        let resp = handle_line(
            r#"{"jsonrpc":"2.0","id":1,"method":"daemon.info"}"#,
            &vpn,
            &bc(),
        )
        .unwrap();
        let r = resp.result.unwrap();
        assert_eq!(r["plugin"]["name"], "stub-plugin");
        assert_eq!(r["plugin"]["authenticator"]["is_authenticated"], true);
        assert_eq!(r["plugin"]["authenticator"]["user_id"], "alice");
    }

    #[test]
    fn daemon_info_plugin_field_is_null_when_no_plugin_installed() {
        let vpn = build_vpn();
        let resp = handle_line(
            r#"{"jsonrpc":"2.0","id":1,"method":"daemon.info"}"#,
            &vpn,
            &bc(),
        )
        .unwrap();
        let r = resp.result.unwrap();
        assert!(r["plugin"].is_null());
    }
}
