//! Default hook wiring — call once at startup.
//!
//! Pushes the standard set of built-in hooks onto a [`VpnManager`]'s pipelines.
//! The composition root (`app/main.rs`, `cli/main.rs`) calls this after
//! constructing the manager, or skips it and wires hooks selectively.
//!
//! ```rust,ignore
//! let mgr = VpnManager::new(registry, storage);
//! service::defaults::register(&mgr); // logging + config-content + validation
//! ```
//!
//! This is a convenience — not a requirement. Every hook pushed here can also
//! be pushed individually, omitted, or replaced by a custom implementation.
//!
//! # Hook registration order
//!
//! For each pipeline, hooks are registered in this order:
//!
//! 1. **`LoggingHook`** — registers first so its `before` fires earliest
//!    (sees the raw input before any other hook transforms it) and its
//!    `after`/`on_error` fire last (see the final outcome).
//!
//! 2. **`ConfigContentLoader`** (validate pipeline only) — reads the config
//!    file into `ValidateConfigInput::config_content` for downstream hooks.
//!
//! 3. **`ValidateBeforeStart`** — runs after content is loaded, validates
//!    before the core handler starts the tunnel.

use crate::middleware;
use crate::VpnManager;
use pingle_config_pipeline::processors::{
    ClashApiProcessor, DnsProcessor, LogProcessor, PlatformProcessor, RoutingExclusionsProcessor,
    RulesetCache, RulesetProcessor, StackProcessor,
};
use pingle_config_pipeline::ProcessorPipeline;
use pingle_pipeline_plugin::PipelinePlugin;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

/// Push the standard built-in hooks onto all lifecycle pipelines.
///
/// Registered hooks:
///
/// | Pipeline | Hook | Phase |
/// |----------|------|-------|
/// | connect, disconnect, restart, validate, status | `LoggingHook` | before + after + on_error |
/// | validate | `ConfigContentLoader` | before |
/// | connect, restart | `ValidateBeforeStart` | before |
pub fn register(mgr: &VpnManager) {
    // LoggingHook — shared across all lifecycle pipelines via Arc.
    let logging = Arc::new(middleware::logging::LoggingHook);
    mgr.connect_pipeline()
        .push_hook(Box::new(Arc::clone(&logging)));
    mgr.disconnect_pipeline()
        .push_hook(Box::new(Arc::clone(&logging)));
    mgr.restart_pipeline()
        .push_hook(Box::new(Arc::clone(&logging)));
    mgr.validate_pipeline()
        .push_hook(Box::new(Arc::clone(&logging)));

    // ConfigContentLoader — validate pipeline only.
    // Runs before ValidateBeforeStart so config_content is populated.
    mgr.validate_pipeline().push_hook(Box::new(
        middleware::config_content::ConfigContentLoader::new(),
    ));

    // ValidateBeforeStart — shared between connect and restart.
    let validate = Arc::new(middleware::validate::ValidateBeforeStart::new(
        mgr.registry(),
    ));
    mgr.connect_pipeline()
        .push_hook(Box::new(Arc::clone(&validate)));
    mgr.restart_pipeline()
        .push_hook(Box::new(Arc::clone(&validate)));
}

/// Build the default native config processor pipeline with all seven
/// processors in the canonical order. Used by
/// [`register_strategy_retry`] and exposed for callers that want to
/// customize the order.
///
/// `cache_root` is where [`RulesetProcessor`] stores downloaded rulesets.
/// Typically `<config-dir>/pingle/ruleset-cache` on macOS / Linux and
/// `%APPDATA%\pingle\ruleset-cache` on Windows; the composition root
/// chooses.
pub fn default_processor_pipeline(cache_root: PathBuf) -> Result<ProcessorPipeline, String> {
    let mut pipeline = ProcessorPipeline::new();
    let cache = RulesetCache::new(cache_root)?;
    pipeline
        .push(Box::new(RulesetProcessor::new(cache)))
        .push(Box::new(DnsProcessor::new()))
        .push(Box::new(RoutingExclusionsProcessor::new()))
        .push(Box::new(StackProcessor::new()))
        .push(Box::new(LogProcessor::new()))
        .push(Box::new(ClashApiProcessor::new()))
        .push(Box::new(PlatformProcessor::new()));
    Ok(pipeline)
}

/// Register a [`StrategyRetryWrap`](middleware::strategy_retry::StrategyRetryWrap)
/// on the connect pipeline with the given native processor pipeline,
/// optional pipeline plugin, and shared cancel flag.
///
/// Call this **after** [`register`] so the strategy retry wrap sits
/// outside `ValidateBeforeStart` and `LoggingHook` — wraps run before
/// the inner hook chain, so the wrap intercepts every connect attempt
/// and only invokes validate / logging once per attempt's inner
/// handler call.
///
/// The `cancel` flag is shared with the IPC disconnect path so an
/// in-flight retry can be aborted cleanly.
pub fn register_strategy_retry(
    mgr: &VpnManager,
    pipeline: ProcessorPipeline,
    plugin: Option<Arc<dyn PipelinePlugin>>,
    cancel: Arc<AtomicBool>,
) {
    let wrap = middleware::strategy_retry::StrategyRetryWrap::new(
        mgr.registry(),
        Arc::new(pipeline),
        plugin,
        cancel,
    );
    mgr.connect_pipeline().push_wrap(Box::new(wrap));
}
