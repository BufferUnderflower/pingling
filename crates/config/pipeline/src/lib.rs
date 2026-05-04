//! Generic config processor pipeline for VPN cores.
//!
//! This crate defines the **public** surface of config processing:
//!
//! - The [`ConfigProcessor`] trait — a pure JSON-in/JSON-out transform
//!   with a stable name and a per-attempt request envelope
//! - The [`ProcessorPipeline`] runner that walks processors in order
//!   with optional step instrumentation
//! - A [`PassthroughProcessor`] default — no-op, sane default when
//!   no extensions are loaded
//! - Stable typed slot payloads for downstream WIT/component packages to
//!   provide config middleware through host composition.
//!
//! ## What this crate does NOT own
//!
//! - Concrete processors (dns, ruleset download, routing, stack
//!   tuning). The OSS build has none built in; vendors ship them as
//!   WIT/component packages plugged in by the downstream host.
//! - Retry / strategy iteration logic. That's composition-root
//!   territory: the daemon's `VpnManager::connect_pipeline()` can
//!   push middleware that decides how many times to retry and with
//!   what modifications. In the OSS build there is no built-in
//!   retry; vendors layer it in as a separate wasm hook.
//! - Wire-format protocol shape. That belongs in whichever plugin crate uses
//!   this pipeline.
//!
//! ## Default behavior (no plugins loaded)
//!
//! An empty `ProcessorPipeline` (or one containing only a
//! `PassthroughProcessor`) returns the input config unchanged. The
//! daemon then hands the original config straight to the active
//! `VpnCore::start()`. This is enough for the OSS daemon to function
//! end-to-end with a raw, hand-written sing-box JSON: no magic, no
//! transforms, just "start the core with this file".

pub mod attempt;
pub mod error;
pub mod pipeline;
pub mod processors;
pub mod strategy;

pub use attempt::{AttemptInfo, ConfigRequest};
pub use error::{classify_error, ErrorKind, PreviousError};
pub use pipeline::{ConfigProcessor, ProcessorPipeline, ProcessorStep};
pub use processors::PassthroughProcessor;
pub use slots::{ConfigProcessPayload, CONFIG_PROCESS_WIRE_VERSION};
pub use strategy::{ConnectionStrategy, ResolverType, RetryPolicy, StackType, StrategyPlan};

pub mod slots;
