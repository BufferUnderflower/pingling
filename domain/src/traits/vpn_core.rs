//! VPN core engine contract.
//!
//! Any VPN engine (sing-box, xray, WireGuard, …) implements this trait.
//! The `service` layer, `app` IPC server, and `cli` binary all depend on
//! this trait — never on a concrete engine.
//!
//! # Implementation strategies
//!
//! A core can be backed by any execution model:
//! - **Subprocess**: spawns a binary via `std::process::Command` (e.g. `core-singbox-standalone`)
//! - **FFI / linked library**: calls a C/Go library in-process (e.g. libbox via `extern "C"`)
//! - **In-process mock**: pure Rust, no external binary (e.g. `core-mock`)
//!
//! The trait is intentionally agnostic — it says "start" and "stop", not
//! "spawn a process" or "send SIGTERM".
//!
//! # Adding a new engine
//!
//! 1. Create a `core-<name>/` crate, implement `VpnCore`
//! 2. Register it in `app/src/main.rs` via `registry.register(descriptor, core)`
//! 3. Optionally register capability pipelines (outbound listing, latency testing)
//!    using `VpnManager::set_list_outbounds()` etc.

use crate::errors::VpnError;
use crate::types::{ConnectionState, CoreEvent, CoreInfo, PrerequisiteCheck};

/// Contract for a VPN core engine.
///
/// Covers the lifecycle that every engine must support: start, stop, kill,
/// status, and validation. Capabilities beyond lifecycle (outbound listing,
/// latency testing) are handled by separate pipelines in the service layer.
///
/// All mutating methods take `&mut self` to enforce exclusive access during
/// state transitions.
pub trait VpnCore: Send + Sync {
    // -- lifecycle ----------------------------------------------------------

    /// Start the engine with the given configuration.
    ///
    /// What "start" means depends on the implementation:
    /// - Subprocess core: spawn the binary with config args
    /// - FFI core: call the library's start function
    /// - Mock core: flip an internal state flag
    ///
    /// # Errors
    /// Returns [`VpnError::ProcessStartFailed`] if the engine cannot start.
    /// Returns [`VpnError::InvalidConfiguration`] if the config path is empty.
    fn start(&mut self, config_path: &str) -> Result<(), VpnError>;

    /// Request a graceful stop.
    ///
    /// # Errors
    /// Returns [`VpnError::NotConnected`] if the engine is not running.
    fn stop(&mut self) -> Result<(), VpnError>;

    /// Force-kill the engine immediately.
    ///
    /// # Errors
    /// Returns [`VpnError::NotConnected`] if the engine is not running.
    fn kill(&mut self) -> Result<(), VpnError>;

    /// Convenience: stop then start with the same (or new) config.
    fn restart(&mut self, config_path: &str) -> Result<(), VpnError> {
        if self.status().is_active() {
            self.stop()?;
        }
        self.start(config_path)
    }

    // -- status -------------------------------------------------------------

    /// Current connection state.
    fn status(&self) -> ConnectionState;

    /// Whether the engine is actually running and responsive.
    ///
    /// Implementations should verify liveness beyond just checking in-memory
    /// state — e.g. `try_wait()` for subprocesses, health-check for FFI cores.
    ///
    /// Default: checks if `status()` is `Connected`.
    fn running(&self) -> bool {
        self.status() == ConnectionState::Connected
    }

    /// Metadata about this engine (name, version, supported protocols).
    fn info(&self) -> CoreInfo;

    // -- validation ---------------------------------------------------------

    /// Validate a configuration file without starting the engine.
    ///
    /// Implementations decide how to validate:
    /// - Subprocess core: run `sing-box check -c <path>`
    /// - FFI core: call the library's validation function
    /// - Mock core: return `Ok(())` unconditionally
    ///
    /// This method is called by the `ValidateBeforeStart` middleware, not
    /// by the connect handler directly. Cores that don't support validation
    /// should return `Ok(())`.
    ///
    /// # Errors
    /// Returns [`VpnError::ValidationError`] if the config is invalid.
    fn validate_config(&self, config_path: &str) -> Result<(), VpnError>;

    /// Check that all prerequisites are met (binary exists, library loaded, etc).
    fn check_prerequisites(&self) -> Vec<PrerequisiteCheck>;

    // -- events -------------------------------------------------------------

    /// Subscribe to a stream of core events.
    ///
    /// Returns a receiver that yields [`CoreEvent`] values as the engine runs.
    /// If the core does not support event streaming, returns `None`.
    fn subscribe(&self) -> Option<std::sync::mpsc::Receiver<CoreEvent>>;

    // -- strategy iteration -------------------------------------------------

    /// Default strategy plan for this core kind, as JSON-serialized
    /// `pingle_config_pipeline::StrategyPlan` bytes. Cores that don't
    /// benefit from strategy iteration return `None` (single-attempt,
    /// no-retry passthrough).
    ///
    /// Returned as `Vec<u8>` (not a typed `StrategyPlan`) so `domain`
    /// stays free of any dependency on `pingle-config-pipeline` —
    /// the strategy retry wrap deserializes the bytes back into a
    /// typed plan when needed.
    ///
    /// Default: `None`. Cores that opt in (e.g. `core-libbox-macos`,
    /// `core-libbox-windows`, `core-singbox-standalone`) override
    /// this to return their tuned plan.
    fn default_strategy_plan(&self) -> Option<Vec<u8>> {
        None
    }
}
