//! Integration tests for ConfigLoader using fixture files.
//!
//! SAFETY: All `unsafe { std::env::set_var/remove_var }` calls are guarded
//! by `#[serial]` and RAII guards to prevent concurrent environment mutation
//! and ensure cleanup on panic.

use app_config::{ConfigLoader, PinglingConfig, PluginsConfig};
use serial_test::serial;
use std::path::{Path, PathBuf};

/// RAII guard that removes an env var on drop (even on panic).
struct EnvGuard(&'static str);
impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        unsafe { std::env::set_var(key, value) };
        Self(key)
    }
}
impl Drop for EnvGuard {
    fn drop(&mut self) {
        unsafe { std::env::remove_var(self.0) };
    }
}

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

#[test]
#[serial]
fn load_minimal_yaml() {
    let path = fixtures_dir().join("minimal.yaml");
    let cfg = ConfigLoader::from_file(&path).unwrap();
    assert_eq!(cfg.core_type, "sing-box");
    assert_eq!(cfg.core_config_path, "/etc/sing-box/config.json");
    assert_eq!(cfg.log_level, "info");
    assert!(cfg.tray);
}

#[test]
#[serial]
fn load_full_yaml() {
    let path = fixtures_dir().join("full.yaml");
    let cfg = ConfigLoader::from_file(&path).unwrap();
    assert_eq!(cfg.core_type, "sing-box");
    assert_eq!(cfg.core_binary_path, "/usr/local/bin/sing-box");
    assert_eq!(cfg.core_config_path, "/etc/sing-box/config.json");
    assert_eq!(cfg.log_level, "debug");
    assert_eq!(cfg.listen_addr, "0.0.0.0:9090");
    assert_eq!(cfg.pid_file, "/var/run/pingling.pid");
    assert!(cfg.tray);
    assert!(!cfg.auto_connect);
}

#[test]
#[serial]
fn load_json_fixture() {
    let path = fixtures_dir().join("config.json");
    let cfg = ConfigLoader::from_file(&path).unwrap();
    assert_eq!(cfg.core_type, "xray");
    assert_eq!(cfg.core_binary_path, "/usr/local/bin/xray");
    assert_eq!(cfg.log_level, "warn");
    assert_eq!(cfg.listen_addr, "127.0.0.1:1080");
    assert!(cfg.tray);
    assert!(!cfg.auto_connect);
}

#[test]
#[serial]
fn env_override_on_fixture() {
    let path = fixtures_dir().join("full.yaml");
    let _guard = EnvGuard::set("PINGLING_LOG_LEVEL", "trace");
    let cfg = ConfigLoader::from_file(&path).unwrap();
    assert_eq!(cfg.log_level, "trace");
}

#[test]
#[serial]
fn full_config_eq_default_plus_overrides() {
    let path = fixtures_dir().join("full.yaml");
    let cfg = ConfigLoader::from_file(&path).unwrap();
    let expected = PinglingConfig {
        core_type: "sing-box".into(),
        core_binary_path: "/usr/local/bin/sing-box".into(),
        core_config_path: "/etc/sing-box/config.json".into(),
        log_level: "debug".into(),
        listen_addr: "0.0.0.0:9090".into(),
        pid_file: "/var/run/pingling.pid".into(),
        tray: true,
        auto_connect: false,
        plugins: PluginsConfig::default(),
    };
    assert_eq!(cfg, expected);
}
