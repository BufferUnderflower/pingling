//! Typed payload definitions for every well-known plugin slot.
//!
//! Each slot in [`slot_names`](super::plugin_slot::slot_names) ships
//! with a Rust payload struct here — the *schema* a plugin sees when
//! the host dispatches `slot.<name>.<phase>`. Payloads flow through
//! [`SlotContext`](super::plugin_slot::SlotContext)'s generic
//! `payload` field, so the slot's type is fixed at the adapter call
//! site and both sides serialize/deserialize via the same shape.
//!
//! ## Structure
//!
//! The module is grouped into three sections:
//!
//! 1. **Wired slots** — schemas + call sites both exist. Wiring the
//!    chain just happens. Examples: [`VpnConnectPayload`],
//!    [`VpnDisconnectPayload`].
//!
//! 2. **Scaffolded slots** — schemas exist, no call site yet. The
//!    first caller drops in a one-liner that fires the chain. No
//!    behavior change until that happens. Examples: the rest of
//!    slots 1–11 from the proposal table.
//!
//! 3. **Future stubs** — listed only in comments + slot_names
//!    constants. No schemas until a real need appears. Documented in
//!    `docs/plugin-slots.md` so the design intent survives.
//!
//! ## Wire version convention
//!
//! Each payload pairs with a `WIRE_VERSION` const that gets stamped
//! into [`SlotContext::wire_version`](super::plugin_slot::SlotContext).
//! Bump when you change a payload shape incompatibly — plugins that
//! see a version they don't recognize should return
//! [`SlotOutcome::Error`](super::plugin_slot::SlotOutcome) with a
//! clear "unsupported wire version" message rather than silently
//! misinterpret fields. Start at 1; bump on breakage.

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ===========================================================================
// WIRED — slots 1–3 (real call sites exist in this release)
// ===========================================================================

/// Wire version for [`VpnConnectPayload`]. Bump when the shape changes.
pub const VPN_CONNECT_WIRE_VERSION: u32 = 1;

/// Payload for `slot.vpn.connect.*`.
///
/// Flows with every `VpnManager::connect()` call. Plugins can:
///
/// - `before` — inspect the request and return `Halt` with a
///   populated `result` to refuse the connect (e.g., quota exceeded).
/// - `exec` — the host's connect logic runs here; plugins that want
///   to replace it entirely return `Continue`/`Halt` with a filled
///   `result` field.
/// - `after` — observe the outcome, record metrics, rotate tokens.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VpnConnectPayload {
    /// Active core type at the time of the connect request (e.g. `"sing-box"`).
    pub core_type: String,

    /// Path to the sing-box config file selected for this connect.
    /// `None` when the daemon is running without a persisted config
    /// path yet (rare; in-memory drivers).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_path: Option<String>,

    /// Free-form metadata the daemon wants to forward — strategy
    /// name on retry attempts, attempt number, previous error, etc.
    /// Kept as an untyped `Value` so the service layer can extend
    /// the struct without churning every plugin that just wants the
    /// top-level fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<Value>,

    /// Populated by `exec` / `after` phases once the connect has
    /// been attempted. `None` on `before`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<ConnectResult>,
}

/// Result payload carried in [`VpnConnectPayload::result`] after a
/// connect attempt completes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectResult {
    /// `true` if `VpnCore::start()` returned Ok.
    pub started: bool,
    /// Wall-clock duration spent inside the core start path.
    pub duration_ms: u64,
    /// Error message if `started == false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// ---------------------------------------------------------------------------

/// Wire version for [`VpnDisconnectPayload`].
pub const VPN_DISCONNECT_WIRE_VERSION: u32 = 1;

/// Payload for `slot.vpn.disconnect.*`.
///
/// Mirrors [`VpnConnectPayload`]. Plugins that want to flush
/// session-level metrics or rotate credentials do so from `after`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VpnDisconnectPayload {
    /// Active core type at the time of the disconnect request.
    pub core_type: String,

    /// Free-form reason string from the caller (`"user"`, `"retry"`,
    /// `"idle"`, etc.). Not enumerated so the string space stays
    /// open for future callers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,

    /// Populated by `exec` / `after` once the disconnect finishes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<DisconnectResult>,
}

/// Result carried in [`VpnDisconnectPayload::result`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisconnectResult {
    /// `true` if `VpnCore::stop()` returned Ok.
    pub stopped: bool,
    /// Wall-clock duration spent inside the core stop path.
    pub duration_ms: u64,
    /// Error message if `stopped == false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// ---------------------------------------------------------------------------

// ===========================================================================
// SCAFFOLDED — slots 4–11 (schemas defined, call sites pending)
// ===========================================================================

/// Wire version for [`CoreStartPayload`].
pub const CORE_START_WIRE_VERSION: u32 = 1;

/// Payload for `slot.core.start.*` — fires when a [`VpnCore`] is
/// being booted with a config. Wire site: wrapped around the
/// `core.start(config_path)` call inside `VpnManager::connect`.
/// Status: schema defined, call site TODO (land when first caller
/// needs it).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreStartPayload {
    pub core_type: String,
    pub config_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<CoreLifecycleResult>,
}

/// Wire version for [`CoreStopPayload`].
pub const CORE_STOP_WIRE_VERSION: u32 = 1;

/// Payload for `slot.core.stop.*`. Mirrors [`CoreStartPayload`].
/// Status: schema defined, call site TODO.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreStopPayload {
    pub core_type: String,
    /// Free-form reason the daemon passes along.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<CoreLifecycleResult>,
}

/// Shared result struct used by core start / stop after-phase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreLifecycleResult {
    pub ok: bool,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// ---------------------------------------------------------------------------

/// Wire version for [`ProfileActivatePayload`].
pub const PROFILE_ACTIVATE_WIRE_VERSION: u32 = 1;

/// Payload for `slot.profile.activate.*`. Fires when a profile
/// becomes the active one (either via the `profile.activate` IPC
/// method or via `deeplink.resolve` auto-activation). Status: schema
/// defined, call site TODO.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileActivatePayload {
    pub profile_id: String,
    /// Free-form reason: `"manual"`, `"deeplink"`, `"default"`, etc.
    pub reason: String,
}

/// Wire version for [`ProfilePersistPayload`].
pub const PROFILE_PERSIST_WIRE_VERSION: u32 = 1;

/// Payload for `slot.profile.persist.*`. Fires when a profile is
/// about to be written to persistent storage. Gives plugins a
/// chance to strip secrets, enforce policy, or reject a shape.
/// Status: schema defined, call site TODO.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfilePersistPayload {
    pub profile_id: String,
    /// Opaque profile content as JSON — plugins inspect or transform.
    pub profile: Value,
    /// Where the profile came from: `"manual"`, `"deeplink"`, `"hub"`, ...
    pub source: String,
}

// ---------------------------------------------------------------------------

/// Wire version for [`DaemonStartupPayload`].
pub const DAEMON_STARTUP_WIRE_VERSION: u32 = 1;

/// Payload for `slot.daemon.startup.*`. Fires once during daemon
/// boot, after plugins are loaded but before the IPC server begins
/// accepting connections. Status: schema defined, call site TODO.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonStartupPayload {
    /// Daemon version string (from `env!("CARGO_PKG_VERSION")`).
    pub version: String,
    /// Number of loaded plugins at startup time.
    pub plugin_count: u32,
    /// Registered core types.
    pub core_types: Vec<String>,
}

/// Wire version for [`DaemonShutdownPayload`].
pub const DAEMON_SHUTDOWN_WIRE_VERSION: u32 = 1;

/// Payload for `slot.daemon.shutdown.*`. Fires on graceful shutdown
/// (SIGTERM, Ctrl-C, `daemon.shutdown` IPC method). Status: schema
/// defined, call site TODO.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonShutdownPayload {
    /// What triggered the shutdown: `"signal"`, `"ipc"`, `"timeout"`, ...
    pub trigger: String,
    /// Seconds the daemon was running before shutdown.
    pub uptime_seconds: u64,
}

// ---------------------------------------------------------------------------

/// Wire version for [`OutboundSelectPayload`].
pub const OUTBOUND_SELECT_WIRE_VERSION: u32 = 1;

/// Payload for `slot.outbound.select.*`. Fires when a client asks
/// the daemon to switch outbound. Plugins can substitute the tag
/// before `exec` or record the choice in `after`. Status: schema
/// defined, call site TODO.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboundSelectPayload {
    /// Outbound tag the client requested.
    pub tag: String,
    /// Currently-active outbound tag, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<String>,
}

/// Wire version for [`OutboundTestLatencyPayload`].
pub const OUTBOUND_TEST_LATENCY_WIRE_VERSION: u32 = 1;

/// Payload for `slot.outbound.test_latency.*`. Fires when a client
/// asks for latency to a single outbound. Status: schema defined,
/// call site TODO.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboundTestLatencyPayload {
    pub tag: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<LatencyResult>,
}

/// Result carried in [`OutboundTestLatencyPayload::result`] after the
/// measurement completes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatencyResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rtt_ms: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// ===========================================================================
// FUTURE — slots 12+ — listed as slot_names constants only
// ===========================================================================
//
// The following slots are intentionally NOT given payload structs
// yet — the design intent is preserved in `docs/plugin-slots.md`.
// When a concrete caller materializes, add a payload struct here and
// bump wire version 1:
//
//   netwatch.event        — network state change observer
//   log.emit              — log sink interception
//   update.check          — update channel decision
//   config.validate       — plugin-specific config linting
//   plugin.load           — post-load capability announcement
//
// These are kept as named constants in `slot_names` so adapters
// can reference the exact string without hardcoding it, but the
// daemon currently does not dispatch into them.

// ---------------------------------------------------------------------------
// Tests — serde round-trip sanity
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Generic round-trip helper: serialize, deserialize, assert
    /// equality by reserializing (avoids needing `PartialEq` on
    /// every payload).
    fn round_trip<T>(value: T)
    where
        T: Serialize + for<'de> Deserialize<'de>,
    {
        let json_str = serde_json::to_string(&value).expect("serialize");
        let parsed: T = serde_json::from_str(&json_str).expect("deserialize");
        let reserialized = serde_json::to_string(&parsed).expect("reserialize");
        assert_eq!(json_str, reserialized);
    }

    #[test]
    fn vpn_connect_payload_round_trip_minimal() {
        round_trip(VpnConnectPayload {
            core_type: "sing-box".into(),
            config_path: None,
            hint: None,
            result: None,
        });
    }

    #[test]
    fn vpn_connect_payload_round_trip_with_result() {
        round_trip(VpnConnectPayload {
            core_type: "sing-box".into(),
            config_path: Some("/tmp/config.json".into()),
            hint: Some(json!({"attempt": 2})),
            result: Some(ConnectResult {
                started: true,
                duration_ms: 842,
                error: None,
            }),
        });
    }

    #[test]
    fn vpn_disconnect_payload_round_trip() {
        round_trip(VpnDisconnectPayload {
            core_type: "sing-box".into(),
            reason: Some("user".into()),
            result: Some(DisconnectResult {
                stopped: true,
                duration_ms: 120,
                error: None,
            }),
        });
    }

    #[test]
    fn scaffolded_payloads_round_trip() {
        round_trip(CoreStartPayload {
            core_type: "sing-box".into(),
            config_path: "/tmp/c.json".into(),
            result: None,
        });
        round_trip(CoreStopPayload {
            core_type: "sing-box".into(),
            reason: Some("user".into()),
            result: Some(CoreLifecycleResult {
                ok: true,
                duration_ms: 50,
                error: None,
            }),
        });
        round_trip(ProfileActivatePayload {
            profile_id: "p-1".into(),
            reason: "manual".into(),
        });
        round_trip(ProfilePersistPayload {
            profile_id: "p-1".into(),
            profile: json!({"core_type": "sing-box"}),
            source: "deeplink".into(),
        });
        round_trip(DaemonStartupPayload {
            version: "0.1.3".into(),
            plugin_count: 1,
            core_types: vec!["sing-box".into(), "mock".into()],
        });
        round_trip(DaemonShutdownPayload {
            trigger: "signal".into(),
            uptime_seconds: 3600,
        });
        round_trip(OutboundSelectPayload {
            tag: "ss-jp".into(),
            previous: Some("auto".into()),
        });
        round_trip(OutboundTestLatencyPayload {
            tag: "ss-jp".into(),
            result: Some(LatencyResult {
                rtt_ms: Some(42),
                error: None,
            }),
        });
    }

    #[test]
    fn wire_versions_all_nonzero() {
        // Quick sanity: every wire-version const we export should be
        // at least 1 (0 is reserved for "uninitialized" in case a
        // caller forgets to stamp the context).
        assert!(VPN_CONNECT_WIRE_VERSION >= 1);
        assert!(VPN_DISCONNECT_WIRE_VERSION >= 1);
        assert!(CORE_START_WIRE_VERSION >= 1);
        assert!(CORE_STOP_WIRE_VERSION >= 1);
        assert!(PROFILE_ACTIVATE_WIRE_VERSION >= 1);
        assert!(PROFILE_PERSIST_WIRE_VERSION >= 1);
        assert!(DAEMON_STARTUP_WIRE_VERSION >= 1);
        assert!(DAEMON_SHUTDOWN_WIRE_VERSION >= 1);
        assert!(OUTBOUND_SELECT_WIRE_VERSION >= 1);
        assert!(OUTBOUND_TEST_LATENCY_WIRE_VERSION >= 1);
    }
}
