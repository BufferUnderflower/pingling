//! Stable wire-protocol identifiers shared between every pingle client.
//!
//! Every JSON-RPC method name and every push-event method name lives here
//! exactly once. Both this Rust file and its Dart twin
//! (`clients/tui/lib/ipc/protocol_constants.dart`) MUST stay in lockstep —
//! `clients/tui/test/protocol_parity_test.dart` is the canary that catches
//! any drift.
//!
//! ## Conventions
//!
//! - Method names follow `<namespace>.<verb>` (`vpn.connect`, `core.list`).
//! - Push events use `event.<thingThatChanged>` (`event.stateChanged`).
//! - When you add a new identifier here, add the matching Dart entry too,
//!   and run `dart test test/protocol_parity_test.dart` before merging.
//!
//! ## Why constants instead of an enum
//!
//! `serde_json::Value::String` interop is simpler with `&'static str`, and
//! the current dispatcher dispatches on `req.method.as_str()`. We could
//! switch to a strum-derived enum later if the surface grows.

#![allow(dead_code)]

/// JSON-RPC methods clients call (request → response).
pub mod methods {
    // VPN lifecycle
    pub const VPN_CONNECT: &str = "vpn.connect";
    pub const VPN_DISCONNECT: &str = "vpn.disconnect";
    pub const VPN_RESTART: &str = "vpn.restart";
    pub const VPN_STATUS: &str = "vpn.status";

    // Core registry & introspection
    pub const CORE_LIST: &str = "core.list";
    pub const CORE_ACTIVE: &str = "core.active";
    pub const CORE_SWITCH: &str = "core.switch";
    pub const CORE_INFO: &str = "core.info";
    pub const CORE_PREREQS: &str = "core.prereqs";
    pub const CORE_CAPABILITIES: &str = "core.capabilities";

    // System extension lifecycle & inspection.
    pub const SYSTEM_EXTENSION_STATUS: &str = "systemExtension.status";
    pub const SYSTEM_EXTENSION_INSTALL: &str = "systemExtension.install";
    pub const SYSTEM_EXTENSION_UNINSTALL: &str = "systemExtension.uninstall";

    // macOS privacy / settings shortcuts.
    pub const SYSTEM_SETTINGS_OPEN_FULL_DISK_ACCESS: &str = "systemSettings.openFullDiskAccess";

    // Settings & config
    pub const CONFIG_GET: &str = "config.get";
    pub const CONFIG_SET: &str = "config.set";
    pub const CONFIG_INFO: &str = "config.info";
    pub const CONFIG_VALIDATE: &str = "config.validate";

    // Outbounds (capability-gated)
    pub const OUTBOUNDS_LIST: &str = "outbounds.list";
    pub const OUTBOUNDS_SELECT: &str = "outbounds.select";
    pub const OUTBOUNDS_TEST_LATENCY: &str = "outbounds.testLatency";

    // NB: under the new plugin architecture, plugin-defined method names
    // (e.g. `auth.login`, `profile.bootstrap`) are NOT listed here. The
    // daemon does not enumerate or validate plugin namespaces — they're
    // handled by the IPC fall-through in `methods.rs`. See
    // `docs/architecture-plugin.md`. The Dart twin
    // (`clients/tui/lib/ipc/protocol_constants.dart`) likewise contains
    // only the daemon-built-in methods; plugin-side method names are
    // hardcoded inside the screens that call them.

    // Daemon meta
    pub const DAEMON_INFO: &str = "daemon.info";
    pub const DAEMON_PING: &str = "daemon.ping";
    pub const DAEMON_INSTALL_ID: &str = "daemon.installId";

    // Deep-link handler. Called by the app's deep-link receiver
    // (tauri-plugin-deep-link on macOS/Windows) with the raw
    // `pingle://...` URL. Also callable directly by IPC clients for
    // testing + programmatic imports.
    pub const DEEPLINK_HANDLE: &str = "deeplink.handle";

    // Profile management — encrypted profile store. Higher-priority
    // config source than the legacy `config_path` setting.
    //
    // Profiles are write-only from clients: you can `put` them, `activate`
    // them, `delete` them, but `get` only returns metadata — the
    // plaintext config body never leaves the daemon over IPC.
    pub const PROFILE_LIST: &str = "profile.list";
    pub const PROFILE_GET: &str = "profile.get";
    pub const PROFILE_PUT: &str = "profile.put";
    pub const PROFILE_DELETE: &str = "profile.delete";
    pub const PROFILE_ACTIVE: &str = "profile.active";
    pub const PROFILE_ACTIVATE: &str = "profile.activate";
    pub const PROFILE_CLEAR_ACTIVE: &str = "profile.clearActive";

    // Subscription handshake (no-op — every connection is auto-subscribed)
    pub const EVENT_SUBSCRIBE: &str = "event.subscribe";
    pub const EVENT_UNSUBSCRIBE: &str = "event.unsubscribe";

    /// All method identifiers, in declaration order. Used by parity tests.
    pub const ALL: &[&str] = &[
        VPN_CONNECT,
        VPN_DISCONNECT,
        VPN_RESTART,
        VPN_STATUS,
        CORE_LIST,
        CORE_ACTIVE,
        CORE_SWITCH,
        CORE_INFO,
        CORE_PREREQS,
        CORE_CAPABILITIES,
        SYSTEM_EXTENSION_STATUS,
        SYSTEM_EXTENSION_INSTALL,
        SYSTEM_EXTENSION_UNINSTALL,
        SYSTEM_SETTINGS_OPEN_FULL_DISK_ACCESS,
        CONFIG_GET,
        CONFIG_SET,
        CONFIG_INFO,
        CONFIG_VALIDATE,
        OUTBOUNDS_LIST,
        OUTBOUNDS_SELECT,
        OUTBOUNDS_TEST_LATENCY,
        DAEMON_INFO,
        DAEMON_PING,
        DAEMON_INSTALL_ID,
        DEEPLINK_HANDLE,
        PROFILE_LIST,
        PROFILE_GET,
        PROFILE_PUT,
        PROFILE_DELETE,
        PROFILE_ACTIVE,
        PROFILE_ACTIVATE,
        PROFILE_CLEAR_ACTIVE,
        EVENT_SUBSCRIBE,
        EVENT_UNSUBSCRIBE,
    ];
}

/// Push event method names (daemon → client notifications).
pub mod events {
    pub const STATE_CHANGED: &str = "event.stateChanged";
    pub const CONFIG_CHANGED: &str = "event.configChanged";
    pub const CONFIG_VALIDATED: &str = "event.configValidated";
    pub const CORE_CHANGED: &str = "event.coreChanged";
    pub const OUTBOUND_SELECTED: &str = "event.outboundSelected";
    pub const LOG: &str = "event.log";
    /// Emitted when any profile changes: created, updated, deleted,
    /// or activated/deactivated. Clients refresh their profile list
    /// in response.
    pub const PROFILE_CHANGED: &str = "event.profileChanged";

    // NB: plugin-side push events (login/logout/etc.) are not declared
    // here for the same reason as plugin-side method names — the daemon
    // does not enumerate the plugin's vocabulary. Plugins broadcast
    // events through their own ipc namespace.

    /// All push-event identifiers, in declaration order. Used by parity tests.
    pub const ALL: &[&str] = &[
        STATE_CHANGED,
        CONFIG_CHANGED,
        CONFIG_VALIDATED,
        CORE_CHANGED,
        OUTBOUND_SELECTED,
        LOG,
        PROFILE_CHANGED,
    ];
}
