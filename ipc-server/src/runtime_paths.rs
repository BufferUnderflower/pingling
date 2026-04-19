//! JSON helpers for the daemon's runtime filesystem paths.

use serde_json::{json, Value};

/// Build the runtime path summary returned over IPC.
pub fn runtime_paths_json() -> Value {
    let paths = util::paths::RuntimePaths::current();
    json!({
        "config_root": paths.config_root,
        "cache_root": paths.cache_root,
        "settings_file": paths.settings_file,
        "profiles_dir": paths.profiles_dir,
        "plugins_dir": paths.plugins_dir,
        "plugin_state_dir": paths.plugin_state_dir,
        "ruleset_cache_dir": paths.ruleset_cache_dir,
        "config_inspect_dir": paths.config_inspect_dir,
        "registry_dir": paths.registry_dir,
        "log_file": paths.log_file,
        "active_config_temp_dir": paths.active_config_temp_dir,
    })
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
    fn runtime_paths_json_contains_expected_fields() {
        let home = TempDir::new().expect("runtime tempdir");
        let _guards = install_runtime_env(home.path());

        let value = runtime_paths_json();
        let config_root = expected_config_root(home.path());
        let cache_root = expected_cache_root(home.path());
        assert_eq!(value["config_root"], config_root.to_string_lossy().as_ref());
        assert_eq!(value["cache_root"], cache_root.to_string_lossy().as_ref());
        assert!(value["settings_file"].is_string());
        assert!(value["profiles_dir"].is_string());
        assert!(value["ruleset_cache_dir"].is_string());
        assert!(value["config_inspect_dir"].is_string());
        assert!(value["registry_dir"].is_string());
        assert!(value["log_file"].is_string());
    }
}
