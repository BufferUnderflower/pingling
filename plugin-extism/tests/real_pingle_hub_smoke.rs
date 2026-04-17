//! Manual smoke test for the **real** Pingle hub wasm plugin.
//!
//! Reads the wasm path from `PINGLE_HUB_WASM` env var; SKIPs if unset.
//! Verifies:
//!
//! 1. The wasm file loads via `PluginAdapter::load` (proves it
//!    exports `plugin_handle_ipc` so `looks_like_plugin` accepts it).
//! 2. The optional `plugin_authenticator_status` export is present
//!    and reports `is_authenticated: false` on a fresh load.
//! 3. The dispatcher claims the plugin's vocabulary
//!    (`auth.session`) without making any network call — proves the
//!    plugin's internal router is wired correctly.
//!
//! This test does NOT make any HTTPS requests — `auth.login` /
//! `profile.bootstrap` need real credentials and network reach to
//! `example.com`, which is reserved for the end-to-end
//! daemon-driven test in `tests/end_to_end.sh`.
//!
//! ## Usage
//!
//! ```sh
//! export PINGLE_HUB_WASM="$HOME/Development/Vladislav/gitlab/groups/pingle_software/client/app/wasm/pingle-hub-userapi/target/wasm32-unknown-unknown/release/pingle_hub_userapi.wasm"
//! cargo test -p plugin-extism --test real_pingle_hub_smoke -- --nocapture
//! ```

use std::path::PathBuf;
use std::{fs, path::Path};

use plugin_extism::plugin_adapter::PluginAdapter;

#[test]
fn real_pingle_hub_wasm_loads_and_exposes_authenticator() {
    let Ok(wasm_path) = std::env::var("PINGLE_HUB_WASM") else {
        eprintln!(
            "SKIP: set PINGLE_HUB_WASM to the path of the built pingle_hub_userapi.wasm \
             to enable this test"
        );
        return;
    };
    let path = PathBuf::from(wasm_path);
    if !path.exists() {
        eprintln!(
            "SKIP: PINGLE_HUB_WASM path does not exist: {}",
            path.display()
        );
        return;
    }

    let plugin = PluginAdapter::load(&path, vec!["example.com".to_string()])
        .expect("PluginAdapter::load on the real wasm should succeed");
    eprintln!("loaded plugin: {}", plugin.name());

    // Authenticator probe — the wasm exports plugin_authenticator_status so
    // this should be Some(...). On a fresh load there is no cached token,
    // so is_authenticated() must be false.
    let auth = plugin
        .authenticator()
        .expect("real plugin exposes authenticator");
    assert!(
        !auth.is_authenticated(),
        "fresh plugin should not report a session"
    );
    assert!(
        auth.user_id().is_none(),
        "fresh plugin should have no user_id"
    );

    // The plugin's auth.session method is a no-network read of the cache.
    // It should claim the method and return logged_in: false.
    let session = plugin
        .handle_ipc("auth.session", &serde_json::Value::Null)
        .expect("auth.session is claimed by the real plugin")
        .expect("auth.session returns ok");
    assert_eq!(session["logged_in"], false);

    // An unknown method must fall through cleanly so the daemon's
    // dispatcher can return MethodNotFound.
    assert!(
        plugin
            .handle_ipc("definitely.not.a.real.method", &serde_json::Value::Null)
            .is_none(),
        "unknown methods must fall through"
    );
}

#[test]
fn real_pingle_hub_wasm_restores_persisted_session_from_disk() {
    let Ok(wasm_path) = std::env::var("PINGLE_HUB_WASM") else {
        eprintln!(
            "SKIP: set PINGLE_HUB_WASM to the path of the built pingle_hub_userapi.wasm \
             to enable this test"
        );
        return;
    };
    let path = PathBuf::from(wasm_path);
    if !path.exists() {
        eprintln!(
            "SKIP: PINGLE_HUB_WASM path does not exist: {}",
            path.display()
        );
        return;
    }

    let plugin_name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .expect("real wasm path should have a file stem");
    let temp_root = tempfile::tempdir().expect("create temp plugin-state root");
    let plugin_dir = temp_root.path().join(plugin_name);
    fs::create_dir_all(&plugin_dir).expect("create plugin session dir");
    let session_path = plugin_dir.join("session.json");
    fs::write(
        &session_path,
        serde_json::json!({
            "token": "restored-token",
            "account_id": "acct-42",
            "display_name": "Restored User",
        })
        .to_string(),
    )
    .expect("seed persisted session");

    let old_override = std::env::var_os("PINGLE_PLUGIN_SESSION_STATE_DIR");
    std::env::set_var("PINGLE_PLUGIN_SESSION_STATE_DIR", temp_root.path());

    let plugin = PluginAdapter::load(&path, vec!["example.com".to_string()])
        .expect("PluginAdapter::load on the real wasm should succeed");

    let auth = plugin
        .authenticator()
        .expect("real plugin exposes authenticator");
    assert!(auth.is_authenticated(), "persisted session should hydrate");
    assert_eq!(auth.user_id().as_deref(), Some("acct-42"));

    let session = plugin
        .handle_ipc("auth.session", &serde_json::Value::Null)
        .expect("auth.session is claimed by the real plugin")
        .expect("auth.session returns ok");
    assert_eq!(session["logged_in"], true);
    assert_eq!(session["account_id"], "acct-42");
    assert_eq!(session["display_name"], "Restored User");
    assert_eq!(session["token"], "restored-token");

    let logged_out = plugin
        .handle_ipc("auth.logout", &serde_json::Value::Null)
        .expect("auth.logout is claimed by the real plugin")
        .expect("auth.logout returns ok");
    assert_eq!(logged_out["ok"], true);
    assert!(
        !Path::new(&session_path).exists(),
        "logout should remove the persisted session file"
    );

    match old_override {
        Some(value) => std::env::set_var("PINGLE_PLUGIN_SESSION_STATE_DIR", value),
        None => std::env::remove_var("PINGLE_PLUGIN_SESSION_STATE_DIR"),
    }
}
