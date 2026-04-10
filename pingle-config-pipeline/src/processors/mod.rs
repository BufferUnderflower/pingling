//! Native config processors — direct ports of the dart
//! `singbox_config` package's processor classes.
//!
//! Each processor is a single struct implementing
//! [`crate::pipeline::ConfigProcessor`]. They are pure JSON-in / JSON-out
//! transforms — no async, no I/O — except for [`ruleset::RulesetProcessor`]
//! which owns the bulletproof native ruleset downloader (the only
//! processor that touches the network).
//!
//! ## Why a native pipeline (not wasm)
//!
//! Sing-box's own ruleset fetcher is flaky on 20–50% of users on Windows
//! — it silently retries with backoff and times out unpredictably. The
//! native ruleset downloader here is bulletproof: own HTTP client, own
//! on-disk cache, own retry on the download itself. By the time a config
//! reaches any core, every `rule_set` entry is `local` pointing into
//! the cache.

pub mod clash_api;
pub mod dns;
pub mod log;
pub mod platform;
pub mod routing_excl;
pub mod ruleset;
pub mod stack;

pub use clash_api::ClashApiProcessor;
pub use dns::DnsProcessor;
pub use log::LogProcessor;
pub use platform::PlatformProcessor;
pub use routing_excl::RoutingExclusionsProcessor;
pub use ruleset::{RulesetCache, RulesetProcessor};
pub use stack::StackProcessor;
