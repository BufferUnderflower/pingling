//! Wasmtime Component runtime defaults for Pingling hosts.

use pingling_host_contract::{render_wit_world, IpcPackageDescriptor};

pub const DEFAULT_WASMTIME_TARGET: &str = "pulley64";
pub const WASMTIME_TARGET_ENV: &str = "PINGLING_WASMTIME_TARGET";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ComponentRuntimeConfig {
    target: String,
}

impl Default for ComponentRuntimeConfig {
    fn default() -> Self {
        Self {
            target: std::env::var(WASMTIME_TARGET_ENV)
                .unwrap_or_else(|_| DEFAULT_WASMTIME_TARGET.to_owned()),
        }
    }
}

impl ComponentRuntimeConfig {
    pub fn new(target: impl Into<String>) -> Self {
        Self {
            target: target.into(),
        }
    }

    pub fn target(&self) -> &str {
        &self.target
    }

    pub fn wasmtime_config(&self) -> Result<wasmtime::Config, String> {
        let mut config = wasmtime::Config::new();
        config.wasm_component_model(true);
        config
            .target(&self.target)
            .map_err(|error| format!("configure Wasmtime target {}: {error}", self.target))?;
        Ok(config)
    }

    pub fn engine(&self) -> Result<wasmtime::Engine, String> {
        wasmtime::Engine::new(&self.wasmtime_config()?)
            .map_err(|error| format!("create Wasmtime engine for {}: {error}", self.target))
    }
}

#[derive(Clone, Debug)]
pub struct ComponentPackageRuntime {
    package: IpcPackageDescriptor,
    config: ComponentRuntimeConfig,
    wit: String,
}

impl ComponentPackageRuntime {
    pub fn prepare(package: IpcPackageDescriptor) -> Result<Self, String> {
        Self::prepare_with_config(package, ComponentRuntimeConfig::default())
    }

    pub fn prepare_with_config(
        package: IpcPackageDescriptor,
        config: ComponentRuntimeConfig,
    ) -> Result<Self, String> {
        package.validate().map_err(|error| error.to_string())?;
        let component = package
            .component
            .as_ref()
            .ok_or_else(|| format!("package {} is missing component descriptor", package.id))?;
        config.wasmtime_config()?;
        let wit = render_wit_world(component).map_err(|error| error.to_string())?;
        Ok(Self {
            package,
            config,
            wit,
        })
    }

    pub fn package_id(&self) -> &str {
        &self.package.id
    }

    pub fn package(&self) -> &IpcPackageDescriptor {
        &self.package
    }

    pub fn config(&self) -> &ComponentRuntimeConfig {
        &self.config
    }

    pub fn wit(&self) -> &str {
        &self.wit
    }
}
