//! Native config processor pipeline + strategy iteration types for the
//! pingle daemon.
//!
//! ## What this crate owns
//!
//! - The [`strategy`] module: `ConnectionStrategy`, `RetryPolicy`,
//!   `StrategyPlan`, `StackType`, `ResolverType`. Direct port of the
//!   dart `ConnectionStrategy` + `RetryPolicy` types.
//! - The [`attempt`] module: `AttemptInfo`, `ConfigRequest` — the
//!   envelope passed through the pipeline and surfaced to plugins.
//! - The [`error`] module: `ErrorKind`, `PreviousError`, `classify_error()`
//!   — the daemon's classification of `VpnError`s into the small stable
//!   taxonomy that drives the strategy retry decision table.
//! - The [`pipeline`] module: `ConfigProcessor` trait + `ProcessorPipeline`
//!   runner with optional step instrumentation.
//! - The [`processors`] module: the seven native processors ported from
//!   the dart `singbox_config` package — `dns`, `ruleset` (with native
//!   download + cache), `routing_excl`, `stack`, `log`, `clash_api`,
//!   `platform`.
//!
//! ## What this crate does NOT own
//!
//! - The plugin slot. That's `pingle-pipeline-plugin`.
//! - The retry orchestrator. That's `service::middleware::strategy_retry`.
//! - The wire-format protocol shape. That's documented in the design spec.
//!
//! See the design spec for the full architecture:
//! `docs/superpowers/specs/2026-04-08-pingle-netwatch-config-pipeline-design.md`

pub mod attempt;
pub mod error;
pub mod pipeline;
pub mod processors;
pub mod strategy;

pub use attempt::{AttemptInfo, ConfigRequest};
pub use error::{classify_error, ErrorKind, PreviousError};
pub use pipeline::{ConfigProcessor, ProcessorPipeline, ProcessorStep};
pub use strategy::{ConnectionStrategy, ResolverType, RetryPolicy, StackType, StrategyPlan};
