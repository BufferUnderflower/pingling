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
//!
//! # What this crate does NOT register
//!
//! - **Concrete config processors** — the public [`core_config_processor`]
//!   crate only ships a passthrough + extism slot. Vendors add real
//!   processors (DNS rewriting, ruleset download, routing exclusions,
//!   stack tuning, etc.) by loading a wasm plugin that claims the
//!   `config.process` method.
//!
//! - **Strategy / retry middleware** — the OSS build has no retry
//!   orchestrator. The connect pipeline calls the core once, directly.
//!   Vendors that want smart retry iterate via a separate wasm hook
//!   layered on top of the connect pipeline.

use crate::middleware;
use crate::VpnManager;
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
