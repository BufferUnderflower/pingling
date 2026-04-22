//! `pingling_domain::Plugin` adapter for wasm plugins loaded via extism.
//!
//! Wraps a [`crate::ExtismPlugin`] in a struct that implements
//! [`pingling_domain::Plugin`] by JSON-marshalling each trait method into a
//! corresponding wasm export. The plugin doesn't need to know
//! anything about the daemon's internal types — it just sees JSON in,
//! JSON out, with field shapes documented in
//! `docs/architecture-plugin.md`.
//!
//! ## Wire contract — only TWO required exports
//!
//! The whole point of the new architecture is that the daemon does
//! not enumerate plugin endpoints. The wasm wire surface is the same
//! shape regardless of how many endpoints the plugin claims:
//!
//! | Wasm export                  | Trait method                  | Input                                            | Output                                                                                          |
//! |------------------------------|-------------------------------|--------------------------------------------------|-------------------------------------------------------------------------------------------------|
//! | `plugin_handle_ipc`          | `Plugin::handle_ipc`          | `{"method": "...", "params": <any json>}`       | `{"handled": true, "result": <json>}` / `{"handled": true, "error": "msg"}` / `{"handled": false}` |
//! | `plugin_authenticator_status`| `Authenticator::is_authenticated` + `user_id` | `null`                                           | `{"is_authenticated": bool, "user_id": "..."}` (optional — absent export = no authenticator)    |
//!
//! - **`plugin_handle_ipc` is REQUIRED.** A wasm file that doesn't
//!   export it is not a Plugin and the discovery loop rejects it
//!   with a clean error.
//! - **`plugin_authenticator_status` is OPTIONAL.** Its absence
//!   means the plugin doesn't manage user identity at all (e.g. an
//!   observability-only plugin) and `Plugin::authenticator()`
//!   returns `None`.
//!
//! Adding a new endpoint to a plugin requires **zero** changes to
//! this file, the daemon, or the trait. The plugin author just adds
//! a new arm inside their `plugin_handle_ipc` router and the daemon
//! transparently forwards client calls to it.
//!
//! ## Why JSON instead of a binary protocol
//!
//! Same reason `plugin-extism` uses JSON for the hook plugins: it's
//! the simplest possible contract for plugin authors in any
//! language. Extism supports msgpack / protobuf via its `convert`
//! module if a future plugin needs the perf, but for the user-facing
//! shape (one call per user action) the encoding cost is invisible
//! next to the network round trip.

use crate::{default_plugin_runtime_config, ExtismPlugin, LoadOptions};
use log::warn;
use pingling_domain::{Authenticator, Plugin, VpnError};
use pingling_host_contract::PluginManifest;
use pingling_host_runtime as runtime;
use pingling_paths::RuntimeLayout;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// The single required wasm export. Anything missing this export is
/// not a Plugin and is skipped by the discovery loop.
const REQUIRED_FUNCTION: &str = "plugin_handle_ipc";
const OPTIONAL_MANIFEST_FUNCTION: &str = "plugin_manifest";

/// Optional export. If present the adapter advertises an
/// [`Authenticator`] sub-interface; if absent, [`Plugin::authenticator`]
/// returns `None`.
const OPTIONAL_AUTHENTICATOR_FUNCTION: &str = "plugin_authenticator_status";
const SESSION_STORE_ENV: &str = "PINGLING_PLUGIN_STATE_DIR";
const SESSION_STORE_CONFIG_KEY: &str = "plugin_session_store_path";
const SESSION_STORE_GUEST_ROOT: &str = "/pingling/plugin-state";

/// Inspects a `.wasm` file and returns `true` if it appears to
/// implement the plugin contract (exports `plugin_handle_ipc`).
/// Used by the daemon's plugin discovery loop to pick the right
/// plugin out of a directory of mixed `.wasm` files.
pub fn looks_like_plugin(plugin: &ExtismPlugin) -> bool {
    plugin.has_function(REQUIRED_FUNCTION)
}

/// Load a plugin manifest either from the optional `plugin_manifest`
/// export or from a sidecar file next to the wasm.
///
/// Sidecar lookup checks `<name>.manifest.json` first and then
/// `<name>.json`. Both are additive to the legacy ABI: missing
/// manifest returns `Ok(None)` so one-plugin deployments keep
/// working while registry mode is introduced.
pub fn load_plugin_manifest(
    plugin_path: &Path,
    plugin: Option<&ExtismPlugin>,
) -> Result<Option<PluginManifest>, String> {
    if let Some(plugin) = plugin.filter(|plugin| plugin.has_function(OPTIONAL_MANIFEST_FUNCTION)) {
        let manifest: PluginManifest = plugin
            .call_json(OPTIONAL_MANIFEST_FUNCTION, &serde_json::Value::Null)
            .map_err(|error| format!("plugin {} manifest export: {error}", plugin.name()))?;
        manifest
            .validate()
            .map_err(|error| format!("plugin {} manifest export: {error}", plugin.name()))?;
        return Ok(Some(manifest));
    }

    load_sidecar_manifest(plugin_path)
}

pub fn load_sidecar_manifest(plugin_path: &Path) -> Result<Option<PluginManifest>, String> {
    for candidate in manifest_sidecar_candidates(plugin_path) {
        if !candidate.exists() {
            continue;
        }
        let raw = fs::read_to_string(&candidate)
            .map_err(|error| format!("read plugin manifest {}: {error}", candidate.display()))?;
        let manifest: PluginManifest = serde_json::from_str(&raw)
            .map_err(|error| format!("parse plugin manifest {}: {error}", candidate.display()))?;
        manifest.validate().map_err(|error| {
            format!("validate plugin manifest {}: {error}", candidate.display())
        })?;
        return Ok(Some(manifest));
    }
    Ok(None)
}

fn manifest_sidecar_candidates(plugin_path: &Path) -> Vec<PathBuf> {
    vec![
        plugin_path.with_extension("manifest.json"),
        plugin_path.with_extension("json"),
    ]
}

/// Adapter that turns an [`ExtismPlugin`] into an `Arc<dyn Plugin>`
/// ready to be installed on
/// [`service::VpnManager::set_plugin`](https://docs.rs/service).
///
/// Construct via [`PluginAdapter::load`] which builds the manifest
/// with HTTPS allow-listing for whichever hosts the plugin author's
/// remote backend lives at.
pub struct PluginAdapter {
    plugin: ExtismPlugin,
    /// Cached snapshot of the authenticator status, refreshed on
    /// every read. Held behind a Mutex because the wasm Plugin
    /// instance is single-threaded and we need a quick read path
    /// for `Authenticator::is_authenticated()` (which is a `&self`
    /// method called from the daemon's IPC layer on every frame).
    auth_cache: Mutex<AuthCache>,
}

#[derive(Default, Clone)]
struct AuthCache {
    is_authenticated: bool,
    user_id: Option<String>,
}

impl PluginAdapter {
    /// Load a wasm plugin from disk and adapt it to the [`Plugin`]
    /// trait. Whitelists the given hostnames so the plugin's
    /// `extism_pdk::http::request` calls can reach them; without
    /// this every HTTPS call from inside wasm fails.
    pub fn load(path: &Path, allowed_hosts: Vec<String>) -> Result<Arc<dyn Plugin>, String> {
        Ok(Self::load_adapter(path, allowed_hosts)? as Arc<dyn Plugin>)
    }

    /// Load a wasm plugin and return the concrete adapter. Registry hosts use
    /// this when they need the public `pingling-host-runtime::Plugin` trait.
    pub fn load_adapter(path: &Path, allowed_hosts: Vec<String>) -> Result<Arc<Self>, String> {
        Self::load_adapter_with_layout(path, allowed_hosts, &RuntimeLayout::for_app("pingling"))
    }

    /// Load a wasm plugin with an explicit runtime layout. Product hosts should
    /// pass their own layout so plugin persistence lives under their configured
    /// state roots instead of the public reference defaults.
    pub fn load_adapter_with_layout(
        path: &Path,
        allowed_hosts: Vec<String>,
        layout: &RuntimeLayout,
    ) -> Result<Arc<Self>, String> {
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();
        let (host_state_dir, guest_state_dir, session_store_path) =
            resolve_session_state_paths(&name, layout)?;
        let opts = LoadOptions {
            allowed_hosts,
            config: default_plugin_runtime_config()
                .into_iter()
                .chain(std::iter::once((
                    SESSION_STORE_CONFIG_KEY.to_string(),
                    session_store_path,
                )))
                .collect(),
            allowed_paths: vec![(
                host_state_dir.to_string_lossy().to_string(),
                guest_state_dir,
            )],
            timeout_ms: Some(30_000),
        };
        let plugin = ExtismPlugin::load_with_options(path, opts)?;
        if !looks_like_plugin(&plugin) {
            return Err(format!(
                "plugin {} does not export `{}`",
                plugin.name(),
                REQUIRED_FUNCTION
            ));
        }
        Ok(Arc::new(Self {
            plugin,
            auth_cache: Mutex::new(AuthCache::default()),
        }))
    }

    /// Re-query the wasm `plugin_authenticator_status` export and
    /// update the in-process cache. Returns the fresh snapshot.
    /// `None` means the plugin does not export the function at all
    /// (and therefore has no authenticator).
    fn refresh_authenticator(&self) -> Option<AuthCache> {
        if !self.plugin.has_function(OPTIONAL_AUTHENTICATOR_FUNCTION) {
            return None;
        }
        let result: Result<AuthStatusWire, _> = self
            .plugin
            .call_json(OPTIONAL_AUTHENTICATOR_FUNCTION, &serde_json::Value::Null);
        match result {
            Ok(s) => {
                let snapshot = AuthCache {
                    is_authenticated: s.is_authenticated,
                    user_id: s.user_id,
                };
                *self.auth_cache.lock().unwrap_or_else(|e| e.into_inner()) = snapshot.clone();
                Some(snapshot)
            }
            Err(e) => {
                warn!(
                    "{} {}: {e}",
                    self.plugin.name(),
                    OPTIONAL_AUTHENTICATOR_FUNCTION
                );
                None
            }
        }
    }
}

fn resolve_session_state_paths(
    plugin_name: &str,
    layout: &RuntimeLayout,
) -> Result<(PathBuf, PathBuf, String), String> {
    let host_root = resolve_plugin_state_root(layout);
    let host_dir = host_root.join(plugin_name);
    fs::create_dir_all(&host_dir).map_err(|e| {
        format!(
            "create plugin session state dir {}: {e}",
            host_dir.display()
        )
    })?;

    let guest_dir = PathBuf::from(format!("{SESSION_STORE_GUEST_ROOT}/{plugin_name}"));
    let session_store_path = guest_dir.join("session.json").to_string_lossy().to_string();
    Ok((host_dir, guest_dir, session_store_path))
}

fn resolve_plugin_state_root(layout: &RuntimeLayout) -> PathBuf {
    if let Some(override_root) = std::env::var_os(SESSION_STORE_ENV)
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
    {
        return override_root;
    }

    layout.plugin_state_root.clone()
}

// ---------------------------------------------------------------------------
// JSON wire types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct HandleIpcInput<'a> {
    method: &'a str,
    params: &'a serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct HandleIpcOutput {
    /// Whether the plugin claims this method. `false` → daemon falls
    /// back to `MethodNotFound`. `true` → either `result` or `error`
    /// is populated.
    handled: bool,
    /// Successful result payload — opaque to the daemon.
    #[serde(default)]
    result: Option<serde_json::Value>,
    /// Plugin-side error message. Surfaced to the client as
    /// `VpnError::Unknown(message)`.
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AuthStatusWire {
    is_authenticated: bool,
    #[serde(default)]
    user_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Plugin impl
// ---------------------------------------------------------------------------

impl Plugin for PluginAdapter {
    fn name(&self) -> &str {
        self.plugin.name()
    }

    fn authenticator(&self) -> Option<&dyn Authenticator> {
        if self.plugin.has_function(OPTIONAL_AUTHENTICATOR_FUNCTION) {
            // Refresh-on-read so the daemon's UI hint is never stale.
            // The Authenticator trait object's lifetime is tied to
            // `self` so we just hand back a `&dyn Authenticator`
            // pointing at us.
            self.refresh_authenticator();
            Some(self)
        } else {
            None
        }
    }

    fn handle_ipc(
        &self,
        method: &str,
        params: &serde_json::Value,
    ) -> Option<Result<serde_json::Value, VpnError>> {
        let input = HandleIpcInput { method, params };
        let result: Result<HandleIpcOutput, String> =
            self.plugin.call_json(REQUIRED_FUNCTION, &input);
        match result {
            Ok(out) if !out.handled => None,
            Ok(out) => Some(match out.error {
                Some(msg) => Err(VpnError::Unknown(msg)),
                None => Ok(out.result.unwrap_or(serde_json::Value::Null)),
            }),
            Err(e) => Some(Err(VpnError::Unknown(format!(
                "wasm plugin {}: {e}",
                self.plugin.name()
            )))),
        }
    }
}

impl runtime::Plugin for PluginAdapter {
    fn name(&self) -> &str {
        self.plugin.name()
    }

    fn handle_ipc(
        &self,
        method: &str,
        params: &serde_json::Value,
    ) -> Option<pingling_host_contract::HostResult<serde_json::Value>> {
        <Self as Plugin>::handle_ipc(self, method, params).map(|result| {
            result.map_err(|error| {
                pingling_host_contract::HostFailure::plugin_error(error.to_string())
            })
        })
    }
}

impl Authenticator for PluginAdapter {
    fn is_authenticated(&self) -> bool {
        // The cache is refreshed in `Plugin::authenticator()`
        // immediately before this method is called, so a stale read
        // window is at most one call deep. Reads from the cache are
        // a Mutex lock + bool copy, no IPC.
        self.auth_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_authenticated
    }

    fn user_id(&self) -> Option<String> {
        self.auth_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .user_id
            .clone()
    }
}

// ---------------------------------------------------------------------------
// Tests — wire-format unit tests with fabricated JSON
// (the wasm-driven end-to-end test lives in
// `tests/plugin_adapter_smoke.rs` and exercises the full path)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn handle_ipc_input_serialises_method_and_params() {
        let params = serde_json::json!({"q": 1});
        let input = HandleIpcInput {
            method: "stub.echo",
            params: &params,
        };
        let v = serde_json::to_value(&input).unwrap();
        assert_eq!(v["method"], "stub.echo");
        assert_eq!(v["params"]["q"], 1);
    }

    #[test]
    fn handle_ipc_output_unhandled_passes_through() {
        let raw = serde_json::json!({"handled": false});
        let out: HandleIpcOutput = serde_json::from_value(raw).unwrap();
        assert!(!out.handled);
        assert!(out.result.is_none());
        assert!(out.error.is_none());
    }

    #[test]
    fn handle_ipc_output_handled_with_result() {
        let raw = serde_json::json!({
            "handled": true,
            "result": {"token": "tok"},
        });
        let out: HandleIpcOutput = serde_json::from_value(raw).unwrap();
        assert!(out.handled);
        assert_eq!(out.result.unwrap()["token"], "tok");
        assert!(out.error.is_none());
    }

    #[test]
    fn handle_ipc_output_handled_with_error() {
        let raw = serde_json::json!({
            "handled": true,
            "error": "boom",
        });
        let out: HandleIpcOutput = serde_json::from_value(raw).unwrap();
        assert!(out.handled);
        assert!(out.result.is_none());
        assert_eq!(out.error.as_deref(), Some("boom"));
    }

    #[test]
    fn auth_status_wire_round_trip_with_user_id_omitted() {
        let raw = serde_json::json!({"is_authenticated": true});
        let s: AuthStatusWire = serde_json::from_value(raw).unwrap();
        assert!(s.is_authenticated);
        assert!(s.user_id.is_none());
    }

    #[test]
    fn auth_status_wire_round_trip_with_user_id_present() {
        let raw = serde_json::json!({
            "is_authenticated": true,
            "user_id": "alice",
        });
        let s: AuthStatusWire = serde_json::from_value(raw).unwrap();
        assert!(s.is_authenticated);
        assert_eq!(s.user_id.as_deref(), Some("alice"));
    }

    #[test]
    fn resolve_plugin_state_root_honours_override() {
        let tmp = tempdir().unwrap();
        let layout = RuntimeLayout::for_app("demo-host");
        unsafe {
            std::env::set_var(SESSION_STORE_ENV, tmp.path());
        }
        let resolved = resolve_plugin_state_root(&layout);
        unsafe {
            std::env::remove_var(SESSION_STORE_ENV);
        }
        assert_eq!(resolved, tmp.path());
    }

    #[test]
    fn resolve_plugin_state_root_uses_runtime_layout() {
        let tmp = tempdir().unwrap();
        let layout = RuntimeLayout::from_roots("demo-host", tmp.path(), tmp.path(), tmp.path());
        let resolved = resolve_plugin_state_root(&layout);
        assert_eq!(resolved, tmp.path().join("plugins"));
    }

    #[test]
    fn sidecar_manifest_prefers_manifest_json() {
        let tmp = tempdir().unwrap();
        let wasm = tmp.path().join("demo-plugin.wasm");
        fs::write(&wasm, b"").unwrap();
        fs::write(
            tmp.path().join("demo-plugin.json"),
            r#"{"id":"fallback.plugin"}"#,
        )
        .unwrap();
        fs::write(
            tmp.path().join("demo-plugin.manifest.json"),
            r#"{
                "id":"example.demo.identity-userapi",
                "priority":100,
                "methods":["auth.*"],
                "slots":[{"name":"auth.session","phases":["exec"],"policy":"single_owner"}],
                "allowed_hosts":["api.example.test"]
            }"#,
        )
        .unwrap();

        let manifest = load_sidecar_manifest(&wasm).unwrap().expect("manifest");

        assert_eq!(manifest.id, "example.demo.identity-userapi");
        assert_eq!(manifest.priority, 100);
        assert_eq!(manifest.methods, vec!["auth.*"]);
    }

    #[test]
    fn sidecar_manifest_missing_is_none() {
        let tmp = tempdir().unwrap();
        let wasm = tmp.path().join("missing.wasm");

        assert!(load_sidecar_manifest(&wasm).unwrap().is_none());
    }
}
