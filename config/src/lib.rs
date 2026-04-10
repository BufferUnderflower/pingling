//! Pingle multi-source configuration.
//!
//! Loads [`PinglingConfig`] from YAML/JSON files and environment variables
//! with a priority cascade: explicit file path > env vars > default search paths.
//!
//! Used by both the Tauri headless daemon (`app`) and the CLI binary (`cli`).
//! The Flutter UI sends config changes to the daemon via JSON-RPC (`config.setPath`);
//! the daemon stores them in [`data::TauriStoreSettings`] and reloads as needed.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Top-level Pingling configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PinglingConfig {
    /// VPN core engine type: "sing-box", "xray", etc.
    #[serde(default = "default_core_type")]
    pub core_type: String,

    /// Path to the VPN core binary.
    /// If empty, searches PATH.
    #[serde(default)]
    pub core_binary_path: String,

    /// Path to the VPN core configuration file.
    /// Passed to the core's `start` and `validate_config` methods.
    #[serde(default)]
    pub core_config_path: String,

    /// Log level: "trace", "debug", "info", "warn", "error".
    #[serde(default = "default_log_level")]
    pub log_level: String,

    /// Daemon listen address (for future IPC).
    #[serde(default = "default_listen_addr")]
    pub listen_addr: String,

    /// PID file path (for future daemon mode).
    #[serde(default)]
    pub pid_file: String,

    /// Enable system tray (for Tauri app layer).
    #[serde(default = "default_true")]
    pub tray: bool,

    /// Auto-connect on startup.
    #[serde(default)]
    pub auto_connect: bool,

    /// Plugin configuration.
    #[serde(default)]
    pub plugins: PluginsConfig,
}

/// Plugin system configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginsConfig {
    /// Directory containing .wasm plugin files.
    /// If empty, plugin system is disabled.
    #[serde(default)]
    pub plugins_dir: String,

    /// List of plugin filenames to load (e.g. ["observability.wasm", "policy.wasm"]).
    /// If empty, all .wasm files in plugins_dir are loaded.
    #[serde(default)]
    pub enabled: Vec<String>,
}

fn default_core_type() -> String {
    "sing-box".into()
}
fn default_log_level() -> String {
    "info".into()
}
fn default_listen_addr() -> String {
    "127.0.0.1:8624".into()
}
fn default_true() -> bool {
    true
}

impl Default for PinglingConfig {
    fn default() -> Self {
        Self {
            core_type: default_core_type(),
            core_binary_path: String::new(),
            core_config_path: String::new(),
            log_level: default_log_level(),
            listen_addr: default_listen_addr(),
            pid_file: String::new(),
            tray: true,
            auto_connect: false,
            plugins: PluginsConfig::default(),
        }
    }
}

/// Loads and merges configuration from multiple sources.
///
/// Priority (highest wins):
/// 1. Environment variables (`PINGLING_*`)
/// 2. Config file (YAML or JSON)
/// 3. Built-in defaults
pub struct ConfigLoader;

/// Parse a string as a boolean environment variable value.
///
/// Returns `true` for non-empty values except "0" and "false" (case-insensitive).
fn parse_bool_env(value: &str) -> bool {
    !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false")
}

impl ConfigLoader {
    /// Apply `PINGLING_*` environment variable overrides to a config.
    fn apply_env_overrides(cfg: &mut PinglingConfig) {
        for (key, value) in std::env::vars() {
            if let Some(rest) = key.strip_prefix("PINGLING_") {
                match rest {
                    "CORE_TYPE" => cfg.core_type = value,
                    "CORE_BINARY_PATH" => cfg.core_binary_path = value,
                    "CORE_CONFIG_PATH" => cfg.core_config_path = value,
                    "LOG_LEVEL" => cfg.log_level = value,
                    "LISTEN_ADDR" => cfg.listen_addr = value,
                    "PID_FILE" => cfg.pid_file = value,
                    "TRAY" => cfg.tray = parse_bool_env(&value),
                    "AUTO_CONNECT" => cfg.auto_connect = parse_bool_env(&value),
                    _ => {}
                }
            }
        }
    }

    /// Load from a specific config file + env var overrides.
    pub fn from_file(path: &Path) -> Result<PinglingConfig, String> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("yaml")
            .to_lowercase();

        let format = match ext.as_str() {
            "yml" | "yaml" => config_rs::FileFormat::Yaml,
            "json" => config_rs::FileFormat::Json,
            _ => return Err(format!("unsupported config format: {ext}")),
        };

        let defaults_yaml = serde_yaml::to_string(&PinglingConfig::default())
            .map_err(|e| format!("serialize defaults: {e}"))?;

        let builder = config_rs::Config::builder()
            .add_source(config_rs::File::from_str(
                &defaults_yaml,
                config_rs::FileFormat::Yaml,
            ))
            .add_source(config_rs::File::from(path).format(format));

        let cfg_raw = builder.build().map_err(|e| format!("build config: {e}"))?;

        let mut cfg: PinglingConfig = cfg_raw
            .try_deserialize()
            .map_err(|e| format!("deserialize config: {e}"))?;

        Self::apply_env_overrides(&mut cfg);

        Ok(cfg)
    }

    /// Load from default locations + env var overrides.
    ///
    /// Search order:
    /// - macOS: `~/Library/Application Support/pingle/config.{yaml,json}`
    /// - Windows: `%APPDATA%\pingle\config.{yaml,json}`
    /// - Linux: `$XDG_CONFIG_HOME/pingle/config.{yaml,json}`, then `/etc/pingle/`
    ///
    /// Falls back to defaults + env if no file found.
    pub fn from_env() -> Result<PinglingConfig, String> {
        let default_paths = Self::default_search_paths();

        for path in &default_paths {
            if path.exists() {
                return Self::from_file(path);
            }
        }

        Self::from_env_only()
    }

    /// Load from environment variables and defaults only (no file).
    pub fn from_env_only() -> Result<PinglingConfig, String> {
        let mut cfg = PinglingConfig::default();
        Self::apply_env_overrides(&mut cfg);
        Ok(cfg)
    }

    /// Returns the default config file search paths.
    ///
    /// On macOS: `~/Library/Application Support/pingle/config.{yaml,json}`
    /// On Windows: `%APPDATA%\pingle\config.{yaml,json}`
    /// On Linux: `$XDG_CONFIG_HOME/pingle/config.{yaml,json}`, then `/etc/pingle/`
    pub fn default_search_paths() -> Vec<PathBuf> {
        let mut paths = Vec::new();

        if let Some(config_dir) = dirs::config_dir() {
            let app_dir = config_dir.join("pingle");
            paths.push(app_dir.join("config.yaml"));
            paths.push(app_dir.join("config.json"));
        }

        #[cfg(unix)]
        {
            paths.push(PathBuf::from("/etc/pingle/config.yaml"));
            paths.push(PathBuf::from("/etc/pingle/config.json"));
        }

        paths
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::fs;
    use tempfile::TempDir;

    /// RAII guard that removes an env var on drop (ensures cleanup on panic).
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

    fn temp_dir() -> TempDir {
        tempfile::tempdir().unwrap()
    }

    fn write_file(dir: &TempDir, name: &str, content: &str) -> PathBuf {
        let path = dir.path().join(name);
        fs::write(&path, content).unwrap();
        path
    }

    // -- defaults -------------------------------------------------------------

    #[test]
    #[serial]
    fn default_config_values() {
        let cfg = PinglingConfig::default();
        assert_eq!(cfg.core_type, "sing-box");
        assert_eq!(cfg.core_binary_path, "");
        assert_eq!(cfg.core_config_path, "");
        assert_eq!(cfg.log_level, "info");
        assert_eq!(cfg.listen_addr, "127.0.0.1:8624");
        assert_eq!(cfg.pid_file, "");
        assert!(cfg.tray);
        assert!(!cfg.auto_connect);
    }

    #[test]
    #[serial]
    fn from_env_only_returns_defaults() {
        let cfg = ConfigLoader::from_env_only().unwrap();
        assert_eq!(cfg.core_type, "sing-box");
        assert_eq!(cfg.log_level, "info");
    }

    // -- YAML loading ---------------------------------------------------------

    #[test]
    #[serial]
    fn load_yaml_partial() {
        let dir = temp_dir();
        let path = write_file(
            &dir,
            "config.yaml",
            "core_type: sing-box\ncore_config_path: /etc/sing/config.json\n",
        );
        let cfg = ConfigLoader::from_file(&path).unwrap();
        assert_eq!(cfg.core_type, "sing-box");
        assert_eq!(cfg.core_config_path, "/etc/sing/config.json");
        assert_eq!(cfg.log_level, "info"); // default
    }

    #[test]
    #[serial]
    fn load_yaml_full() {
        let dir = temp_dir();
        let path = write_file(
            &dir,
            "config.yaml",
            r#"
core_type: xray
core_binary_path: /usr/bin/xray
core_config_path: /etc/xray/config.json
log_level: debug
listen_addr: "0.0.0.0:9090"
pid_file: /tmp/xray.pid
tray: false
auto_connect: true
"#,
        );
        let cfg = ConfigLoader::from_file(&path).unwrap();
        assert_eq!(cfg.core_type, "xray");
        assert_eq!(cfg.core_binary_path, "/usr/bin/xray");
        assert_eq!(cfg.log_level, "debug");
        assert_eq!(cfg.listen_addr, "0.0.0.0:9090");
        assert!(!cfg.tray);
        assert!(cfg.auto_connect);
    }

    #[test]
    #[serial]
    fn load_yaml_with_comments() {
        let dir = temp_dir();
        let path = write_file(
            &dir,
            "config.yaml",
            "# Pingling config\ncore_type: sing-box\n# log_level: warn\n",
        );
        let cfg = ConfigLoader::from_file(&path).unwrap();
        assert_eq!(cfg.core_type, "sing-box");
        assert_eq!(cfg.log_level, "info"); // default
    }

    #[test]
    #[serial]
    fn load_yaml_yml_extension() {
        let dir = temp_dir();
        let path = write_file(&dir, "config.yml", "core_type: sing-box\n");
        let cfg = ConfigLoader::from_file(&path).unwrap();
        assert_eq!(cfg.core_type, "sing-box");
    }

    // -- JSON loading ---------------------------------------------------------

    #[test]
    #[serial]
    fn load_json_partial() {
        let dir = temp_dir();
        let path = write_file(
            &dir,
            "config.json",
            r#"{"core_type": "xray", "log_level": "warn"}"#,
        );
        let cfg = ConfigLoader::from_file(&path).unwrap();
        assert_eq!(cfg.core_type, "xray");
        assert_eq!(cfg.log_level, "warn");
        assert_eq!(cfg.listen_addr, "127.0.0.1:8624"); // default
    }

    #[test]
    #[serial]
    fn load_json_full() {
        let dir = temp_dir();
        let path = write_file(
            &dir,
            "config.json",
            r#"{
                "core_type": "xray",
                "core_binary_path": "/usr/local/bin/xray",
                "log_level": "error",
                "tray": false,
                "auto_connect": true
            }"#,
        );
        let cfg = ConfigLoader::from_file(&path).unwrap();
        assert_eq!(cfg.core_type, "xray");
        assert_eq!(cfg.core_binary_path, "/usr/local/bin/xray");
        assert_eq!(cfg.log_level, "error");
        assert!(!cfg.tray);
        assert!(cfg.auto_connect);
    }

    // -- env var overrides ----------------------------------------------------

    #[test]
    #[serial]
    fn env_vars_override() {
        let _g1 = EnvGuard::set("PINGLING_LOG_LEVEL", "debug");
        let _g2 = EnvGuard::set("PINGLING_CORE_TYPE", "xray");
        let cfg = ConfigLoader::from_env_only().unwrap();
        assert_eq!(cfg.log_level, "debug");
        assert_eq!(cfg.core_type, "xray");
    }

    // -- merge priority -------------------------------------------------------

    #[test]
    #[serial]
    fn file_overrides_defaults() {
        let dir = temp_dir();
        let path = write_file(&dir, "config.yaml", "log_level: warn\n");
        let cfg = ConfigLoader::from_file(&path).unwrap();
        assert_eq!(cfg.log_level, "warn");
    }

    #[test]
    #[serial]
    fn env_overrides_file() {
        let dir = temp_dir();
        let path = write_file(&dir, "config.yaml", "log_level: warn\n");
        let _guard = EnvGuard::set("PINGLING_LOG_LEVEL", "error");
        let cfg = ConfigLoader::from_file(&path).unwrap();
        assert_eq!(cfg.log_level, "error");
    }

    #[test]
    #[serial]
    fn explicit_overrides_env() {
        let _guard = EnvGuard::set("PINGLING_LOG_LEVEL", "error");
        let mut cfg = ConfigLoader::from_env_only().unwrap();
        cfg.log_level = "trace".into();
        assert_eq!(cfg.log_level, "trace");
    }

    // -- search paths ---------------------------------------------------------

    #[test]
    #[serial]
    fn search_paths_are_non_empty() {
        let paths = ConfigLoader::default_search_paths();
        assert!(!paths.is_empty());
        // On unix, /etc/pingle/ should be in the list.
        #[cfg(unix)]
        assert!(paths.iter().any(|p| p.starts_with("/etc/")));
        // On all platforms, at least one path should contain "pingle".
        assert!(paths.iter().any(|p| p.to_string_lossy().contains("pingle")));
    }

    #[test]
    #[serial]
    fn from_env_falls_back_to_defaults() {
        let result = ConfigLoader::from_env();
        assert!(result.is_ok());
    }

    // -- error handling -------------------------------------------------------

    #[test]
    #[serial]
    fn unsupported_format() {
        let dir = temp_dir();
        let path = write_file(&dir, "config.toml", "core_type = 'sing-box'\n");
        let result = ConfigLoader::from_file(&path);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unsupported"));
    }

    #[test]
    #[serial]
    fn invalid_yaml() {
        let dir = temp_dir();
        let path = write_file(&dir, "config.yaml", "{{{{invalid yaml");
        let result = ConfigLoader::from_file(&path);
        assert!(result.is_err());
    }

    #[test]
    #[serial]
    fn missing_file_errors() {
        let result = ConfigLoader::from_file(Path::new("/nonexistent/config.yaml"));
        assert!(result.is_err());
    }

    #[test]
    #[serial]
    fn empty_yaml_file_returns_defaults() {
        let dir = temp_dir();
        let path = write_file(&dir, "config.yaml", "");
        let cfg = ConfigLoader::from_file(&path).unwrap();
        assert_eq!(cfg, PinglingConfig::default());
    }
}
