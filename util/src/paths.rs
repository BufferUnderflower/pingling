//! Shared filesystem paths used by the daemon and its support crates.
//!
//! These helpers centralize the platform-specific root directories so the
//! app, headless daemon, registry writer, profile store, and config
//! processor cache all agree on the same locations.

use std::path::PathBuf;

/// Resolved runtime paths for one daemon installation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimePaths {
    pub config_root: PathBuf,
    pub cache_root: PathBuf,
    pub settings_file: PathBuf,
    pub profiles_dir: PathBuf,
    pub plugins_dir: PathBuf,
    pub plugin_state_dir: PathBuf,
    pub ruleset_cache_dir: PathBuf,
    pub registry_dir: PathBuf,
    pub log_file: PathBuf,
    pub active_config_temp_dir: PathBuf,
}

impl RuntimePaths {
    /// Resolve the current platform's runtime paths.
    pub fn current() -> Self {
        let config_root = app_root(dirs::config_dir().unwrap_or_else(|| std::env::temp_dir()));
        let cache_root = app_root(dirs::cache_dir().unwrap_or_else(|| std::env::temp_dir()));
        Self {
            settings_file: config_root.join("settings.json"),
            profiles_dir: config_root.join("profiles"),
            plugins_dir: config_root.join("plugins"),
            plugin_state_dir: config_root.join("plugin-state"),
            ruleset_cache_dir: cache_root.join("rulesets"),
            registry_dir: cache_root.join("daemons"),
            log_file: cache_root.join("daemon.log"),
            active_config_temp_dir: std::env::temp_dir().join("pingle-active-configs"),
            config_root,
            cache_root,
        }
    }
}

fn app_root(base: PathBuf) -> PathBuf {
    base.join("pingle")
}

/// Root directory for persistent config files.
pub fn config_root() -> PathBuf {
    RuntimePaths::current().config_root
}

/// Root directory for cacheable or ephemeral daemon data.
pub fn cache_root() -> PathBuf {
    RuntimePaths::current().cache_root
}

/// JSON settings file used by the daemon.
pub fn settings_file() -> PathBuf {
    RuntimePaths::current().settings_file
}

/// Encrypted profile directory.
pub fn profiles_dir() -> PathBuf {
    RuntimePaths::current().profiles_dir
}

/// Plugin directory.
pub fn plugins_dir() -> PathBuf {
    RuntimePaths::current().plugins_dir
}

/// Plugin session state directory.
pub fn plugin_state_dir() -> PathBuf {
    RuntimePaths::current().plugin_state_dir
}

/// Ruleset cache directory.
pub fn ruleset_cache_dir() -> PathBuf {
    RuntimePaths::current().ruleset_cache_dir
}

/// Per-daemon registry directory.
pub fn registry_dir() -> PathBuf {
    RuntimePaths::current().registry_dir
}

/// Daemon log file path.
pub fn log_file() -> PathBuf {
    RuntimePaths::current().log_file
}

/// Temp directory for decrypted active configs.
pub fn active_config_temp_dir() -> PathBuf {
    RuntimePaths::current().active_config_temp_dir
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use tempfile::TempDir;

    struct EnvGuard(&'static str);

    impl EnvGuard {
        fn set(key: &'static str, value: &std::path::Path) -> Self {
            unsafe { std::env::set_var(key, value) };
            Self(key)
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe { std::env::remove_var(self.0) };
        }
    }

    fn install_runtime_env(root: &std::path::Path) -> Vec<EnvGuard> {
        let mut guards = Vec::new();
        for key in [
            "HOME",
            "XDG_CONFIG_HOME",
            "XDG_CACHE_HOME",
            "APPDATA",
            "LOCALAPPDATA",
            "TMPDIR",
            "TEMP",
            "TMP",
        ] {
            guards.push(EnvGuard::set(key, root));
        }
        guards
    }

    fn expected_config_root(root: &std::path::Path) -> std::path::PathBuf {
        #[cfg(target_os = "macos")]
        {
            root.join("Library/Application Support/pingle")
        }

        #[cfg(target_os = "windows")]
        {
            root.join("pingle")
        }

        #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
        {
            root.join("pingle")
        }
    }

    fn expected_cache_root(root: &std::path::Path) -> std::path::PathBuf {
        #[cfg(target_os = "macos")]
        {
            root.join("Library/Caches/pingle")
        }

        #[cfg(target_os = "windows")]
        {
            root.join("pingle")
        }

        #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
        {
            root.join("pingle")
        }
    }

    #[test]
    #[serial]
    fn current_runtime_paths_respect_config_and_cache_roots() {
        let home = TempDir::new().expect("runtime tempdir");
        let _guards = install_runtime_env(home.path());

        let paths = RuntimePaths::current();
        let config_root = expected_config_root(home.path());
        let cache_root = expected_cache_root(home.path());
        assert_eq!(paths.config_root, config_root);
        assert_eq!(paths.cache_root, cache_root);
        assert_eq!(paths.settings_file, config_root.join("settings.json"));
        assert_eq!(paths.profiles_dir, config_root.join("profiles"));
        assert_eq!(paths.plugins_dir, config_root.join("plugins"));
        assert_eq!(paths.plugin_state_dir, config_root.join("plugin-state"));
        assert_eq!(paths.ruleset_cache_dir, cache_root.join("rulesets"));
        assert_eq!(paths.registry_dir, cache_root.join("daemons"));
        assert_eq!(paths.log_file, cache_root.join("daemon.log"));
        assert_eq!(
            paths.active_config_temp_dir,
            std::env::temp_dir().join("pingle-active-configs")
        );
    }

    #[test]
    #[serial]
    fn helpers_align_with_runtime_paths() {
        let home = TempDir::new().expect("runtime tempdir");
        let _guards = install_runtime_env(home.path());

        let paths = RuntimePaths::current();
        assert_eq!(config_root(), paths.config_root);
        assert_eq!(cache_root(), paths.cache_root);
        assert_eq!(settings_file(), paths.settings_file);
        assert_eq!(profiles_dir(), paths.profiles_dir);
        assert_eq!(plugins_dir(), paths.plugins_dir);
        assert_eq!(plugin_state_dir(), paths.plugin_state_dir);
        assert_eq!(ruleset_cache_dir(), paths.ruleset_cache_dir);
        assert_eq!(registry_dir(), paths.registry_dir);
        assert_eq!(log_file(), paths.log_file);
        assert_eq!(active_config_temp_dir(), paths.active_config_temp_dir);
    }
}
