//! Extism plugin slot for the pingle config processor pipeline.
//!
//! This crate is the optional, drop-in extension point that lets a
//! wasm guest observe and transform sing-box configs as they flow
//! through the native pipeline. The contract is documented in
//! `docs/superpowers/specs/2026-04-08-pingle-netwatch-config-pipeline-design.md`
//! under "Pipeline plugin protocol".
//!
//! ## Three layers
//!
//! 1. **Wire types** ([`protocol`]) — `PipelineCapabilities`,
//!    `ProcessConfigInput`, `ProcessConfigOutput`, `PipelineStage`,
//!    `CoreInfo`. These match the JSON shapes the daemon and the wasm
//!    guest agree on.
//! 2. **Trait** ([`trait_def`]) — `PipelinePlugin` is the
//!    daemon-facing interface. Implementations may be wasm-backed
//!    (the canonical case) or hand-written for tests.
//! 3. **Extism adapter** ([`extism_plugin`]) — `ExtismPipelinePlugin`
//!    loads a `.wasm` file via the same `extism::Plugin` API as the
//!    existing `plugin-extism` crate, but speaks the pipeline protocol
//!    instead of the user-api hook protocol.
//!
//! ## Passthrough by default
//!
//! Whenever the daemon can't load a plugin (file missing, plugin returns
//! `[]` from `pipeline_capabilities`, or any per-call error), the
//! pipeline falls through to the native processor output unchanged. A
//! misbehaving plugin must NEVER break connect — that's enforced both
//! here (lenient error handling in the adapter) and in
//! `service::middleware::strategy_retry`.

pub mod extism_plugin;
pub mod protocol;
pub mod trait_def;

pub use extism_plugin::ExtismPipelinePlugin;
pub use protocol::{
    CoreInfo, PipelineCapabilities, PipelineStage, ProcessConfigInput, ProcessConfigOutput,
    CANONICAL_STAGE_ORDER, WIRE_VERSION,
};
pub use trait_def::{PipelinePlugin, PluginError};
