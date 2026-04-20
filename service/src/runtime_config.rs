use core_config_processor::{
    AttemptInfo, ConfigRequest, ConnectionStrategy, PreviousError, ResolverType, RetryPolicy,
    StackType,
};
use domain::{TempConfigPath, VpnError};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

static TEMP_CONFIG_COUNTER: AtomicU64 = AtomicU64::new(0);

pub(crate) struct PreparedRuntimeConfig {
    pub(crate) path: String,
    pub(crate) temp_path: TempConfigPath,
}

pub(crate) fn materialize_runtime_config(
    source_path: &str,
    ruleset_cache_dir: &Path,
    active_config_temp_dir: &Path,
    target: core_config_processor_impls::CoreCompatTarget,
    request: &ConfigRequest,
) -> Result<PreparedRuntimeConfig, VpnError> {
    let raw_config = std::fs::read_to_string(source_path).map_err(|e| {
        VpnError::InvalidConfiguration(format!("read runtime config {source_path}: {e}"))
    })?;
    let config: serde_json::Value = serde_json::from_str(&raw_config).map_err(|e| {
        VpnError::InvalidConfiguration(format!("parse runtime config {source_path}: {e}"))
    })?;

    let pipeline = core_config_processor_impls::default_pipeline_for_core(
        ruleset_cache_dir.to_path_buf(),
        target,
    )
    .map_err(|e| VpnError::StorageError(format!("init runtime config processor pipeline: {e}")))?;
    let processed = pipeline.process(config, request).map_err(|e| {
        VpnError::InvalidConfiguration(format!("process runtime config {source_path}: {e}"))
    })?;

    std::fs::create_dir_all(active_config_temp_dir).map_err(|e| {
        VpnError::StorageError(format!(
            "create runtime config temp dir {}: {e}",
            active_config_temp_dir.display()
        ))
    })?;

    let suffix = TEMP_CONFIG_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_nanos();
    let temp_path = active_config_temp_dir.join(format!(
        "runtime-config-{}-{nanos}-{suffix}.json",
        std::process::id()
    ));
    let rendered = serde_json::to_vec_pretty(&processed).map_err(|e| {
        VpnError::StorageError(format!(
            "serialize processed runtime config {source_path}: {e}"
        ))
    })?;
    std::fs::write(&temp_path, rendered).map_err(|e| {
        VpnError::StorageError(format!(
            "write processed runtime config {}: {e}",
            temp_path.display()
        ))
    })?;

    Ok(PreparedRuntimeConfig {
        path: temp_path.to_string_lossy().into_owned(),
        temp_path: TempConfigPath::new(temp_path),
    })
}

pub(crate) fn default_request() -> ConfigRequest {
    request_for_strategy(&default_strategy(), 1, None)
}

pub(crate) fn request_for_strategy(
    strategy: &ConnectionStrategy,
    attempt_number: u32,
    previous_error: Option<PreviousError>,
) -> ConfigRequest {
    ConfigRequest {
        with_host_dns: false,
        default_dns_server: None,
        attempt: AttemptInfo {
            strategy: strategy.clone(),
            attempt_number,
            previous_error,
        },
    }
}

pub(crate) fn default_strategy() -> ConnectionStrategy {
    ConnectionStrategy {
        id: "direct_start".into(),
        stack: StackType::System,
        resolver_type: ResolverType::System,
        total_timeout: Duration::from_secs(30),
        retry: RetryPolicy::NoRetry,
    }
}
