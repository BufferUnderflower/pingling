use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeLayout {
    pub app_name: String,
    pub config_root: PathBuf,
    pub cache_root: PathBuf,
    pub state_root: PathBuf,
    pub plugin_state_root: PathBuf,
    pub profiles_root: PathBuf,
    pub settings_file: PathBuf,
    pub active_config_temp_dir: PathBuf,
}

impl RuntimeLayout {
    pub fn for_app(app_name: impl Into<String>) -> Self {
        let app_name = app_name.into();
        let config_root = platform_config_root(&app_name);
        let cache_root = platform_cache_root(&app_name);
        let state_root = platform_state_root(&app_name);
        Self::from_roots(app_name, config_root, cache_root, state_root)
    }

    pub fn from_roots(
        app_name: impl Into<String>,
        config_root: impl Into<PathBuf>,
        cache_root: impl Into<PathBuf>,
        state_root: impl Into<PathBuf>,
    ) -> Self {
        let app_name = app_name.into();
        let config_root = config_root.into();
        let cache_root = cache_root.into();
        let state_root = state_root.into();
        Self {
            plugin_state_root: state_root.join("plugins"),
            profiles_root: config_root.join("profiles"),
            settings_file: config_root.join("settings.json"),
            active_config_temp_dir: std::env::temp_dir().join(format!("{app_name}-active-configs")),
            app_name,
            config_root,
            cache_root,
            state_root,
        }
    }

    pub fn with_plugin_state_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.plugin_state_root = root.into();
        self
    }

    pub fn with_profiles_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.profiles_root = root.into();
        self
    }

    pub fn with_settings_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.settings_file = path.into();
        self
    }
}

pub fn which(program: &str) -> Option<String> {
    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var).find_map(|dir| {
        let candidate = dir.join(program);
        if candidate.is_file() {
            Some(candidate.to_string_lossy().to_string())
        } else {
            #[cfg(windows)]
            {
                let candidate = dir.join(format!("{program}.exe"));
                if candidate.is_file() {
                    return Some(candidate.to_string_lossy().to_string());
                }
            }
            None
        }
    })
}

pub fn ensure_dir(path: impl AsRef<Path>) -> std::io::Result<()> {
    std::fs::create_dir_all(path)
}

fn platform_config_root(app_name: &str) -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        .join(app_name)
}

fn platform_cache_root(app_name: &str) -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| std::env::temp_dir().join("cache"))
        .join(app_name)
}

fn platform_state_root(app_name: &str) -> PathBuf {
    dirs::data_local_dir()
        .or_else(dirs::data_dir)
        .unwrap_or_else(|| std::env::temp_dir().join("state"))
        .join(app_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_is_app_name_scoped() {
        let layout = RuntimeLayout::from_roots(
            "demo",
            PathBuf::from("/tmp/demo-config"),
            PathBuf::from("/tmp/demo-cache"),
            PathBuf::from("/tmp/demo-state"),
        );

        assert_eq!(
            layout.settings_file,
            PathBuf::from("/tmp/demo-config/settings.json")
        );
        assert_eq!(
            layout.plugin_state_root,
            PathBuf::from("/tmp/demo-state/plugins")
        );
        assert_eq!(
            layout.active_config_temp_dir,
            std::env::temp_dir().join("demo-active-configs")
        );
    }

    #[test]
    fn layout_allows_overrides() {
        let layout = RuntimeLayout::from_roots(
            "demo",
            PathBuf::from("/tmp/c"),
            PathBuf::from("/tmp/k"),
            PathBuf::from("/tmp/s"),
        )
        .with_plugin_state_root("/tmp/plugins")
        .with_settings_file("/tmp/settings.json");

        assert_eq!(layout.plugin_state_root, PathBuf::from("/tmp/plugins"));
        assert_eq!(layout.settings_file, PathBuf::from("/tmp/settings.json"));
    }
}
