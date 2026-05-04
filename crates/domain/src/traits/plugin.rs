//! Generic plugin slot for a Pingling host.
//!
//! The daemon is open-source. Vendor-specific concerns (auth, billing,
//! subscription server lists, panel APIs) live in **plugins**, not in
//! the public daemon code. This module is the void the daemon exposes
//! for those plugins to fill.
//!
//! ## Design rule
//!
//! The trait surface here knows **nothing** about what a plugin does.
//! No method names like `login`, `bootstrap`, `checkout`. No value
//! types like `Wallet`, `Session`, `Order`. The plugin author defines
//! their own vocabulary inside their plugin and the daemon proxies
//! IPC calls to it as opaque (method, params) pairs.
//!
//! Concretely:
//!
//! - The daemon's IPC layer dispatches its built-in `vpn.*`/`core.*`/
//!   `config.*` methods directly. Anything it does NOT recognize, it
//!   forwards to the plugin via [`Plugin::handle_ipc`]. The plugin
//!   either claims the method (`Some(Ok(...))` / `Some(Err(...))`) or
//!   passes (`None`), in which case the daemon returns
//!   `MethodNotFound`.
//! - The plugin **may** expose an [`Authenticator`] sub-interface so
//!   the daemon (and any UI rendered by clients) can ask "is anyone
//!   currently logged in?" without dispatching a full IPC call. This
//!   is the only piece of cross-cutting concern the daemon names.
//!
//! That's the whole interface.
//!
//! ## Why so small
//!
//! Earlier drafts of this trait had typed methods for `login`,
//! `bootstrap`, `list_outbounds`, `baked_config`, `checkout` — and
//! all the value types those return (`AuthMode`, `Session`,
//! `UserBootstrap`, `Wallet`, `Order`, `Checkout`). Every one of
//! those names is **vendor product surface** that does not belong in
//! the public OSS daemon. The daemon should be a void waiting for a
//! plugin to plug in, not a typed dispatch table for one specific
//! panel's API. New endpoints on a plugin must require **zero**
//! daemon changes.
//!
//! See `docs/architecture-plugin.md` for the full rationale.

use crate::VpnError;
use serde_json::Value;

/// A loaded plugin. The daemon holds at most one of these via
/// [`crate::traits::Plugin::set`-style](crate::traits) accessors on
/// the service layer (see `service::VpnManager::set_plugin`).
///
/// Implementors are typically not handwritten Rust types — they are
/// adapters that wrap a WIT-described component guest loaded by the downstream
/// host and translate trait calls into component exports.
///
/// All methods must be safe to call from any thread; the daemon's
/// IPC dispatcher invokes them from worker threads without any
/// further synchronization.
pub trait Plugin: Send + Sync {
    /// Human-readable plugin name. Used in logs and the daemon's
    /// host info IPC method so clients can show the active plugin owner.
    /// or similar in their status bar. Free-form — typically the
    /// `.wasm` file stem for wasm plugins.
    fn name(&self) -> &str;

    /// Optional authenticator sub-interface.
    ///
    /// `None` means the plugin does not manage user identity at all
    /// (e.g. an observability plugin that only forwards events). In
    /// that case the daemon and its clients render an "anonymous"
    /// state and never display login UI.
    ///
    /// `Some(...)` means the plugin handles its own auth flow
    /// internally — login/logout/token-storage are the plugin's
    /// concern, called via [`handle_ipc`](Self::handle_ipc) under
    /// whatever method names the plugin chooses (the daemon does
    /// not name them). The returned trait object is just the
    /// "is anyone logged in right now?" probe the daemon needs for
    /// UI hints.
    fn authenticator(&self) -> Option<&dyn Authenticator>;

    /// Try to handle a JSON-RPC method call.
    ///
    /// `Some(Ok(value))` — plugin claims this method and returns a
    /// successful result; the daemon serialises it as the JSON-RPC
    /// `result` field.
    ///
    /// `Some(Err(err))` — plugin claims this method but the call
    /// failed; the daemon converts to an `APPLICATION_ERROR` RPC
    /// error and returns it to the client.
    ///
    /// `None` — plugin does not recognize this method. The daemon
    /// continues its dispatch chain (currently: returns
    /// `MethodNotFound`; in the future may try other plugins if a
    /// multi-plugin slot is added).
    ///
    /// Method names are the **plugin's** vocabulary. The daemon does
    /// not enumerate them, validate them, or document them — that's
    /// the plugin's responsibility. Clients learn the plugin's
    /// surface from the plugin's own documentation, not from this
    /// trait.
    fn handle_ipc(&self, method: &str, params: &Value) -> Option<Result<Value, VpnError>>;
}

/// Cross-cutting "is the user authenticated right now?" probe.
///
/// This is the **only** piece of plugin-internal state the daemon
/// surfaces in its own trait surface. Everything else lives behind
/// `handle_ipc`. The reason this one breaks the rule: clients want
/// to render "logged in as Alice" / "Login" buttons in their
/// chrome without dispatching a full IPC round trip per frame, and
/// the daemon needs the same boolean to expose in `daemon.info`.
///
/// Plugins that don't manage identity return `None` from
/// [`Plugin::authenticator`] and never implement this trait.
pub trait Authenticator: Send + Sync {
    /// Whether there is currently an authenticated user. This is a
    /// snapshot, not a network call — the implementation should be
    /// cheap (`Cell<bool>` read or similar). The plugin's own
    /// login/logout handlers update the underlying state.
    fn is_authenticated(&self) -> bool;

    /// Optional opaque user identifier for display only. The daemon
    /// passes this through to clients verbatim. Format is the
    /// plugin's choice (numeric id, email, username, etc.).
    /// `None` is fine even when `is_authenticated()` is `true` —
    /// some auth modes (e.g. anonymous bearer tokens) have no user
    /// identifier to show.
    fn user_id(&self) -> Option<String>;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// Smallest possible plugin: no auth, claims no methods. Used to
    /// prove the slot pattern works for an "observability-only"
    /// plugin shape.
    struct NullPlugin;

    impl Plugin for NullPlugin {
        fn name(&self) -> &str {
            "null"
        }
        fn authenticator(&self) -> Option<&dyn Authenticator> {
            None
        }
        fn handle_ipc(&self, _method: &str, _params: &Value) -> Option<Result<Value, VpnError>> {
            None
        }
    }

    /// Plugin with a stub authenticator that always reports
    /// "logged in as alice", and claims one method.
    struct StubPlugin {
        auth: StubAuth,
    }

    struct StubAuth;

    impl Authenticator for StubAuth {
        fn is_authenticated(&self) -> bool {
            true
        }
        fn user_id(&self) -> Option<String> {
            Some("alice".into())
        }
    }

    impl Plugin for StubPlugin {
        fn name(&self) -> &str {
            "stub"
        }
        fn authenticator(&self) -> Option<&dyn Authenticator> {
            Some(&self.auth)
        }
        fn handle_ipc(&self, method: &str, params: &Value) -> Option<Result<Value, VpnError>> {
            match method {
                "stub.echo" => Some(Ok(params.clone())),
                _ => None,
            }
        }
    }

    #[test]
    fn null_plugin_claims_nothing_and_has_no_authenticator() {
        let p: Arc<dyn Plugin> = Arc::new(NullPlugin);
        assert_eq!(p.name(), "null");
        assert!(p.authenticator().is_none());
        assert!(p.handle_ipc("anything", &serde_json::Value::Null).is_none());
    }

    #[test]
    fn stub_plugin_authenticator_reports_logged_in() {
        let p: Arc<dyn Plugin> = Arc::new(StubPlugin { auth: StubAuth });
        let auth = p.authenticator().expect("stub plugin has authenticator");
        assert!(auth.is_authenticated());
        assert_eq!(auth.user_id().as_deref(), Some("alice"));
    }

    #[test]
    fn stub_plugin_claims_only_its_own_methods() {
        let p: Arc<dyn Plugin> = Arc::new(StubPlugin { auth: StubAuth });
        let echoed = p
            .handle_ipc("stub.echo", &serde_json::json!({"hi": "there"}))
            .expect("stub.echo is claimed")
            .expect("stub.echo returns ok");
        assert_eq!(echoed, serde_json::json!({"hi": "there"}));

        // Methods the plugin doesn't recognise return None — the
        // daemon then surfaces MethodNotFound to the client.
        assert!(p
            .handle_ipc("vpn.connect", &serde_json::Value::Null)
            .is_none());
    }

    #[test]
    fn plugin_trait_object_is_send_and_sync() {
        // Compile-time check: trait objects of `Plugin` must be
        // Send + Sync so the daemon can hand them to worker threads
        // without further synchronization.
        fn assert_send_sync<T: Send + Sync + ?Sized>() {}
        assert_send_sync::<dyn Plugin>();
        assert_send_sync::<dyn Authenticator>();
    }
}
