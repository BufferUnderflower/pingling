//! Built-in and example hooks for the VPN pipeline.
//!
//! # Built-in hooks
//!
//! - [`logging`] — [`LoggingHook`](logging::LoggingHook): logs every operation
//!   at `info` level across all three phases (before/after/on_error). Register
//!   first so it captures the full lifecycle including effects of other hooks.
//!
//! - [`validate`] — [`ValidateBeforeStart`](validate::ValidateBeforeStart):
//!   validates the config file before connect/restart (pure `before` hook).
//!   Hooks short-circuit on failure; the handler never runs.
//!
//! - [`config_content`] — [`ConfigContentLoader`](config_content::ConfigContentLoader):
//!   reads the config file into `ValidateConfigInput::config_content` so plugin
//!   hooks can inspect and transform the raw text. Register before `validate`
//!   and plugin hooks. Non-aborting on read failure.
//!
//! # Core-specific handlers (terminal)
//!
//! - [`singbox_config`] — [`SingboxConfigHandler`](singbox_config::SingboxConfigHandler):
//!   parses a sing-box JSON config to extract the outbound list. This is the
//!   *terminal handler* (not a hook) for `Pipeline<OpListOutbounds>` when
//!   sing-box is the active core.
//!
//! # Example hooks (reference implementations)
//!
//! - [`geo_filter`] — [`GeoFilterHook`](geo_filter::GeoFilterHook):
//!   filters outbounds by country whitelist (`after` on `OpListOutbounds`).
//!
//! - [`latency_bias`] — [`LatencyBiasHook`](latency_bias::LatencyBiasHook):
//!   learns country codes from list-outbounds (`after`) then penalizes
//!   non-preferred countries in latency results (`after` on `OpTestLatency`).

pub mod config_content;
pub mod geo_filter;
pub mod latency_bias;
pub mod logging;
pub mod singbox_config;
pub mod validate;
