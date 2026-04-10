//! End-to-end smoke test for `PluginAdapter` against a real wasm module.
//!
//! Builds the `tests/fixtures/plugin_mock` crate to wasm32-unknown-unknown
//! at test time, loads the produced `.wasm` via `PluginAdapter::load`, then
//! exercises every relevant slice of the `Plugin` + `Authenticator` traits.
//!
//! ## Why a build-at-test-time fixture instead of a committed binary?
//!
//! The fixture's source is a few dozen lines and we don't want a wasm
//! binary in source control going stale silently. Building at test
//! time keeps the source the source of truth.
//!
//! ## Skip behaviour
//!
//! If `wasm32-unknown-unknown` is not installed via rustup, the test
//! prints a SKIP message and exits successfully. This keeps CI green
//! on hosts without the wasm target while still being a useful gate
//! for local dev. To enable on a fresh machine:
//!
//! ```sh
//! rustup target add wasm32-unknown-unknown
//! cargo test -p plugin-extism --test plugin_adapter_smoke
//! ```

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;

use plugin_extism::plugin_adapter::PluginAdapter;

/// Locate the rustup-wrapper cargo at `$HOME/.cargo/bin/cargo`.
///
/// **Why this is needed:** the parent workspace pins toolchain 1.94.1
/// via `rust-toolchain.toml` and that toolchain has the
/// `wasm32-unknown-unknown` target installed. But on a typical macOS
/// dev machine `cargo` on PATH may be `/opt/homebrew/bin/cargo`
/// (homebrew rust), which doesn't read `rust-toolchain.toml` and
/// doesn't ship with the wasm32 target. Calling the rustup wrapper
/// directly bypasses both problems.
fn rustup_wrapper_cargo() -> Option<OsString> {
    let home = std::env::var_os("HOME")?;
    let path = PathBuf::from(home).join(".cargo/bin/cargo");
    if path.exists() {
        Some(path.into_os_string())
    } else {
        None
    }
}

/// Returns true iff a rustup toolchain has wasm32-unknown-unknown
/// installed. Uses `rustup target list --installed` against whichever
/// toolchain is active for THIS directory (rustup auto-resolves via
/// rust-toolchain.toml). Returns false if rustup itself is missing.
fn wasm32_target_installed() -> bool {
    let Ok(out) = Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
    else {
        return false;
    };
    if !out.status.success() {
        return false;
    }
    String::from_utf8_lossy(&out.stdout).contains("wasm32-unknown-unknown")
}

/// Build the fixture in release mode and return the path to the
/// produced `.wasm` file.
fn build_fixture() -> Option<PathBuf> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let fixture_dir = manifest_dir.join("tests/fixtures/plugin_mock");
    let target_dir = fixture_dir.join("target");

    let cargo_bin = rustup_wrapper_cargo()
        .or_else(|| std::env::var_os("CARGO"))
        .unwrap_or_else(|| OsString::from("cargo"));

    eprintln!("building fixture with cargo = {}", cargo_bin.to_string_lossy());

    let status = Command::new(&cargo_bin)
        .current_dir(&fixture_dir)
        .env("CARGO_TARGET_DIR", &target_dir)
        .args([
            "build",
            "--release",
            "--target",
            "wasm32-unknown-unknown",
            "--quiet",
        ])
        .status()
        .ok()?;
    if !status.success() {
        eprintln!("fixture build failed");
        return None;
    }
    let wasm = target_dir.join("wasm32-unknown-unknown/release/plugin_mock.wasm");
    if !wasm.exists() {
        eprintln!("expected wasm at {} but file is missing", wasm.display());
        return None;
    }
    Some(wasm)
}

#[test]
fn plugin_adapter_round_trips_handle_ipc_against_real_wasm() {
    if !wasm32_target_installed() {
        eprintln!(
            "SKIP: wasm32-unknown-unknown target not installed. \
             Run `rustup target add wasm32-unknown-unknown` to enable this test."
        );
        return;
    }
    let Some(wasm_path) = build_fixture() else {
        eprintln!("SKIP: fixture build failed (see above)");
        return;
    };

    let plugin = PluginAdapter::load(&wasm_path, vec![])
        .expect("PluginAdapter::load on the mock fixture should succeed");

    // -- handle_ipc: claimed method with success result --------------
    let result = plugin
        .handle_ipc(
            "auth.login",
            &serde_json::json!({"mode": "guest", "extra": "stuff"}),
        )
        .expect("auth.login is claimed by the mock plugin")
        .expect("auth.login returns ok");
    assert_eq!(result["token"], "mock-tok");
    assert_eq!(result["account_id"], "mock-1");
    assert_eq!(result["display_name"], "Mock User");
    assert_eq!(result["is_new"], true);
    // The plugin echoes the params verbatim — proves the daemon's
    // params shape is forwarded opaquely without rename / coercion.
    assert_eq!(result["echoed_params"]["mode"], "guest");
    assert_eq!(result["echoed_params"]["extra"], "stuff");

    // -- handle_ipc: claimed method with the success-no-result shape -
    let result = plugin
        .handle_ipc("auth.logout", &serde_json::Value::Null)
        .expect("auth.logout is claimed")
        .expect("auth.logout returns ok");
    assert_eq!(result["ok"], true);

    // -- handle_ipc: another claimed method with nested data ---------
    let result = plugin
        .handle_ipc("profile.bootstrap", &serde_json::Value::Null)
        .expect("profile.bootstrap is claimed")
        .expect("profile.bootstrap returns ok");
    assert_eq!(result["account_id"], "mock-1");
    assert_eq!(result["wallet"]["balance_units"], 1000);
    assert_eq!(result["features"]["is_mock"], true);

    // -- handle_ipc: claimed method that returns an error envelope ---
    let err = plugin
        .handle_ipc("auth.fail", &serde_json::Value::Null)
        .expect("auth.fail is claimed (with an error)")
        .expect_err("auth.fail returns an error envelope");
    let err_msg = err.to_string();
    assert!(
        err_msg.contains("simulated failure"),
        "expected the plugin's error message to surface; got {err_msg}"
    );

    // -- handle_ipc: unclaimed method falls through ------------------
    assert!(
        plugin
            .handle_ipc("nope.unknown", &serde_json::Value::Null)
            .is_none(),
        "unclaimed methods must return None so the daemon can return MethodNotFound"
    );

    // -- authenticator: optional sub-interface present + reports state
    let auth = plugin
        .authenticator()
        .expect("mock plugin exposes an authenticator");
    assert!(auth.is_authenticated());
    assert_eq!(auth.user_id().as_deref(), Some("mock-1"));
}
