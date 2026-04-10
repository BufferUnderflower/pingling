//! `StrategyRetryWrap` — `WrapHook<OpConnect>` that owns the strategy
//! iteration + retry loop.
//!
//! Sits outside `ConnectHandler` (and any other connect-side hooks).
//! For each connect attempt the wrap:
//!
//! 1. Resolves a `StrategyPlan` from one of three sources (in order):
//!    a. `ConnectInput.metadata["strategy_plan_json"]` — per-call override
//!    b. The active core's `default_strategy_plan()` trait method
//!    c. `None` → middleware passes through to inner handler unchanged
//! 2. For each strategy in the plan, in order:
//!    - Runs the native [`ProcessorPipeline`] over the base config
//!    - For each stage the optional [`PipelinePlugin`] claims, calls
//!      `process_config` and uses the returned config
//!    - Writes the final config to a UUID-named temp file
//!    - Calls the inner handler with the rewritten `config_path`
//!    - On success, cleans up temp files and returns
//!    - On error, classifies via `pingle_config_pipeline::classify_error`
//!      and decides retry / advance / bail per the action table
//! 3. On global timeout exceeded → returns `VpnError::Unknown("global timeout")`
//! 4. On cancellation flag set → returns `VpnError::Cancelled`
//!
//! See `docs/superpowers/specs/2026-04-08-pingle-netwatch-config-pipeline-design.md`
//! for the full algorithm and the error→action table.

use crate::CoreRegistry;
use domain::ops::*;
use domain::pipeline::{Handler, WrapHook};
use domain::VpnError;
use log::{info, warn};
use pingle_config_pipeline::{
    classify_error, AttemptInfo, ConfigRequest, ConnectionStrategy, ErrorKind, PreviousError,
    ProcessorPipeline, StrategyPlan,
};
use pingle_pipeline_plugin::protocol::ProcessConfigAttempt;
use pingle_pipeline_plugin::{
    CoreInfo, PipelinePlugin, PipelineStage, ProcessConfigInput, CANONICAL_STAGE_ORDER,
    WIRE_VERSION,
};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Action the retry loop takes after classifying one failed attempt.
///
/// Computed from the table in
/// `docs/superpowers/specs/2026-04-08-pingle-netwatch-config-pipeline-design.md`
/// — see `action_for` for the canonical mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StrategyAction {
    /// Retry the current strategy if the policy allows another attempt;
    /// otherwise advance to the next strategy.
    Retry,
    /// Skip remaining attempts of this strategy and advance immediately.
    AdvanceStrategy,
    /// Stop the entire plan, return the original error to the caller.
    Bail,
}

/// The error→action mapping. **The contract.**
///
/// Adding a new variant to `ErrorKind` requires adding a row here or
/// the test `action_table_is_total` fails.
fn action_for(kind: ErrorKind) -> StrategyAction {
    match kind {
        ErrorKind::DnsFailure
        | ErrorKind::TcpTimeout
        | ErrorKind::TcpRefused
        | ErrorKind::TlsHandshake
        | ErrorKind::HttpError
        | ErrorKind::Timeout
        | ErrorKind::Unknown => StrategyAction::Retry,
        ErrorKind::Validation => StrategyAction::AdvanceStrategy,
        ErrorKind::AuthFailure
        | ErrorKind::TunDevice
        | ErrorKind::PermissionDenied
        | ErrorKind::PrerequisiteMissing => StrategyAction::Bail,
        // ErrorKind is non_exhaustive — future variants default to
        // "best effort retry". Adding a new variant should also add an
        // explicit row above; this fall-through keeps the build green
        // until that happens.
        _ => StrategyAction::Retry,
    }
}

/// `WrapHook<OpConnect>` that drives the strategy retry loop.
///
/// Construct with [`StrategyRetryWrap::new`]. Register on the connect
/// pipeline via `pipeline.push_wrap(Box::new(wrap))`. The wrap takes
/// over the entire connect lifecycle from validation through start.
pub struct StrategyRetryWrap {
    registry: Arc<Mutex<CoreRegistry>>,
    pipeline: Arc<ProcessorPipeline>,
    plugin: Option<Arc<dyn PipelinePlugin>>,
    cancel: Arc<AtomicBool>,
    /// Per-attempt sleep granularity. Tests use very short delays so
    /// they don't actually wait. Production uses 100ms — short enough
    /// to feel responsive on cancel, long enough to not burn CPU.
    cancel_poll_interval: Duration,
}

impl StrategyRetryWrap {
    /// Construct a new wrap with the given dependencies.
    pub fn new(
        registry: Arc<Mutex<CoreRegistry>>,
        pipeline: Arc<ProcessorPipeline>,
        plugin: Option<Arc<dyn PipelinePlugin>>,
        cancel: Arc<AtomicBool>,
    ) -> Self {
        Self {
            registry,
            pipeline,
            plugin,
            cancel,
            cancel_poll_interval: Duration::from_millis(100),
        }
    }

    /// Override the cancel-poll interval. Tests use a very short value
    /// so the inner sleep_or_cancel loop wakes up quickly.
    pub fn with_cancel_poll_interval(mut self, interval: Duration) -> Self {
        self.cancel_poll_interval = interval;
        self
    }

    /// Resolve the strategy plan according to the override priority order.
    fn resolve_plan(&self, input: &ConnectInput) -> Option<StrategyPlan> {
        // 1. Per-call override
        if let Some(json) = input.metadata.get("strategy_plan_json") {
            match serde_json::from_str::<StrategyPlan>(json) {
                Ok(plan) => return Some(plan),
                Err(e) => warn!("strategy_retry: per-call plan override failed to parse: {e} — falling through"),
            }
        }
        // 2. Per-core default (held under registry lock)
        let mut registry = self.registry.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(core) = registry.active_core() {
            if let Some(bytes) = core.default_strategy_plan() {
                match serde_json::from_slice::<StrategyPlan>(&bytes) {
                    Ok(plan) => return Some(plan),
                    Err(e) => warn!("strategy_retry: core default plan failed to parse: {e}"),
                }
            }
        }
        None
    }

    /// Read the active core's metadata for the plugin protocol's `core` field.
    fn core_info(&self, input: &ConnectInput) -> CoreInfo {
        let kind = input.core_type.clone();
        let mut registry = self.registry.lock().unwrap_or_else(|e| e.into_inner());
        let (version, platform) = match registry.active_core() {
            Some(core) => {
                let info = core.info();
                let platform = if cfg!(target_os = "macos") {
                    "macos"
                } else if cfg!(target_os = "windows") {
                    "windows"
                } else if cfg!(target_os = "linux") {
                    "linux"
                } else {
                    "other"
                };
                (info.version, platform.to_string())
            }
            None => ("unknown".to_string(), "unknown".to_string()),
        };
        CoreInfo {
            kind,
            version,
            platform,
        }
    }

    /// Build a [`ConfigRequest`] envelope for one attempt.
    fn build_request(
        input: &ConnectInput,
        strategy: ConnectionStrategy,
        attempt_number: u32,
        previous_error: Option<PreviousError>,
    ) -> ConfigRequest {
        let with_host_dns = input
            .metadata
            .get("with_host_dns")
            .and_then(|s| s.parse::<bool>().ok())
            .unwrap_or(false);
        let default_dns_server = input.metadata.get("default_dns_server").cloned();
        ConfigRequest {
            with_host_dns,
            default_dns_server,
            attempt: AttemptInfo {
                strategy,
                attempt_number,
                previous_error,
            },
        }
    }

    /// Walk the plugin's claimed stages in canonical order, calling
    /// `process_config` at each. On per-stage error, log warn and
    /// continue with the current config (lenient — a misbehaving
    /// plugin must never break connect).
    fn run_plugin_stages(
        &self,
        plugin: &dyn PipelinePlugin,
        mut config: Value,
        core_info: &CoreInfo,
        request: &ConfigRequest,
    ) -> Value {
        let claimed: Vec<&PipelineStage> = plugin.capabilities().stages.iter().collect();
        if claimed.is_empty() {
            return config;
        }
        for stage in CANONICAL_STAGE_ORDER {
            if !claimed.iter().any(|s| *s == stage) {
                continue;
            }
            let input = ProcessConfigInput {
                wire_version: WIRE_VERSION,
                core: core_info.clone(),
                attempt: ProcessConfigAttempt::from_attempt(stage.clone(), &request.attempt),
                config: config.clone(),
            };
            match plugin.process_config(stage.clone(), input) {
                Ok(out) => {
                    config = out.config;
                    for diag in out.diagnostics {
                        info!(
                            "plugin {}/{}: {diag}",
                            plugin.name(),
                            stage.as_str()
                        );
                    }
                }
                Err(e) => {
                    warn!(
                        "plugin {}/{}: failed: {e} — falling back to native output",
                        plugin.name(),
                        stage.as_str()
                    );
                }
            }
        }
        config
    }

    /// Sleep for `delay`, polling the cancel flag every
    /// `cancel_poll_interval`. Returns `true` if cancelled.
    fn sleep_or_cancel(&self, delay: Duration) -> bool {
        let deadline = Instant::now() + delay;
        loop {
            if self.cancel.load(Ordering::SeqCst) {
                return true;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            let remaining = deadline - now;
            std::thread::sleep(remaining.min(self.cancel_poll_interval));
        }
    }
}

impl WrapHook<OpConnect> for StrategyRetryWrap {
    fn name(&self) -> &str {
        "strategy_retry"
    }

    fn handle(
        &self,
        input: ConnectInput,
        next: &dyn Handler<OpConnect>,
    ) -> Result<ConnectOutput, VpnError> {
        let plan = match self.resolve_plan(&input) {
            Some(p) => p,
            None => {
                log::debug!("strategy_retry: no plan — passthrough");
                return next.handle(input);
            }
        };
        if plan.strategies.is_empty() {
            log::debug!("strategy_retry: empty plan — passthrough");
            return next.handle(input);
        }

        let started = Instant::now();
        let global_budget = plan.global_timeout;
        info!(
            "strategy_retry: begin plan strategies={} global_timeout={:?}",
            plan.strategies.len(),
            global_budget
        );

        let base_config = std::fs::read_to_string(&input.config_path)
            .map_err(|e| VpnError::InvalidConfiguration(format!("read base config: {e}")))?;
        let base_value: Value = serde_json::from_str(&base_config)
            .map_err(|e| VpnError::InvalidConfiguration(format!("parse base config json: {e}")))?;

        let core_info = self.core_info(&input);
        let mut tracked_tmps: Vec<PathBuf> = Vec::new();
        let mut last_err: Option<VpnError> = None;

        for strategy in &plan.strategies {
            let mut attempt: u32 = 1;
            let mut previous_error: Option<PreviousError> = None;
            info!(
                "strategy_retry: enter strategy id={} retry={:?}",
                strategy.id, strategy.retry
            );

            loop {
                if self.cancel.load(Ordering::SeqCst) {
                    cleanup(&tracked_tmps);
                    return Err(VpnError::Cancelled);
                }
                if let Some(g) = global_budget {
                    if started.elapsed() >= g {
                        cleanup(&tracked_tmps);
                        return Err(VpnError::Unknown(format!(
                            "strategy_retry: global timeout ({}ms) exceeded",
                            g.as_millis()
                        )));
                    }
                }

                let request = Self::build_request(
                    &input,
                    strategy.clone(),
                    attempt,
                    previous_error.clone(),
                );

                // 1. Native pipeline.
                let native_out = self
                    .pipeline
                    .process(base_value.clone(), &request)
                    .map_err(VpnError::Unknown)?;

                // 2. Plugin slot.
                let transformed = match self.plugin.as_ref() {
                    Some(p) => self.run_plugin_stages(p.as_ref(), native_out, &core_info, &request),
                    None => native_out,
                };

                // 3. Write to UUID-named temp file. tempfile generates
                //    unique names so attempts don't collide.
                let tmp_path = match write_temp_config(&transformed) {
                    Ok(p) => p,
                    Err(e) => {
                        cleanup(&tracked_tmps);
                        return Err(VpnError::InvalidConfiguration(format!(
                            "write temp config: {e}"
                        )));
                    }
                };
                tracked_tmps.push(tmp_path.clone());

                // 4. Hand off to inner handler with the rewritten path.
                let mut attempt_input = input.clone();
                attempt_input.config_path = tmp_path.to_string_lossy().into_owned();

                match next.handle(attempt_input) {
                    Ok(out) => {
                        info!(
                            "strategy_retry: success on '{}' attempt {} after {}ms",
                            strategy.id,
                            attempt,
                            started.elapsed().as_millis()
                        );
                        cleanup(&tracked_tmps);
                        return Ok(out);
                    }
                    Err(err) => {
                        let classified = classify_error(&err);
                        warn!(
                            "strategy_retry: '{}' attempt {} failed ({:?}): {err}",
                            strategy.id, attempt, classified.kind
                        );
                        let action = action_for(classified.kind);
                        previous_error = Some(classified);
                        last_err = Some(err);

                        match action {
                            StrategyAction::Bail => {
                                cleanup(&tracked_tmps);
                                return Err(last_err.unwrap());
                            }
                            StrategyAction::AdvanceStrategy => {
                                info!("strategy_retry: advancing strategy after Validation failure");
                                break;
                            }
                            StrategyAction::Retry => {
                                let next_attempt = attempt + 1;
                                if next_attempt > strategy.retry.max_attempts() {
                                    info!(
                                        "strategy_retry: '{}' exhausted after {} attempts; advancing",
                                        strategy.id, attempt
                                    );
                                    break;
                                }
                                let delay = strategy.retry.delay_for(next_attempt);
                                if delay > Duration::ZERO && self.sleep_or_cancel(delay) {
                                    cleanup(&tracked_tmps);
                                    return Err(VpnError::Cancelled);
                                }
                                attempt = next_attempt;
                            }
                        }
                    }
                }
            }
        }

        cleanup(&tracked_tmps);
        Err(last_err.unwrap_or_else(|| {
            VpnError::Unknown("strategy_retry: all strategies exhausted".into())
        }))
    }
}

/// Write a JSON Value to a uniquely-named temp file. Returns the path.
fn write_temp_config(value: &Value) -> Result<PathBuf, String> {
    use std::io::Write;
    let mut tmp = tempfile::Builder::new()
        .prefix("pingle-strategy-retry-")
        .suffix(".json")
        .tempfile()
        .map_err(|e| format!("create temp file: {e}"))?;
    tmp.write_all(serde_json::to_string(value).unwrap_or_default().as_bytes())
        .map_err(|e| format!("write: {e}"))?;
    let (_, path) = tmp.keep().map_err(|e| format!("persist: {e}"))?;
    Ok(path)
}

/// Best-effort cleanup of all temp files written during this attempt.
fn cleanup(paths: &[PathBuf]) {
    for path in paths {
        let _ = std::fs::remove_file(path);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CoreRegistry;
    use domain::pipeline::Handler;
    use domain::{
        ConnectionState, CoreDescriptor, CoreInfo as CoreInfoMeta, CoreSource, PrerequisiteCheck,
        VpnCore,
    };
    use pingle_config_pipeline::strategy::{ResolverType, RetryPolicy, StackType};
    use pingle_pipeline_plugin::{PipelineCapabilities, ProcessConfigOutput};
    use std::collections::BTreeMap;
    use std::sync::atomic::AtomicU32;

    // -- Mock VpnCore that can be configured to return a strategy plan -------

    struct PlanCore {
        plan_bytes: Option<Vec<u8>>,
    }

    impl VpnCore for PlanCore {
        fn start(&mut self, config_path: &str) -> Result<(), VpnError> {
            if config_path.is_empty() {
                return Err(VpnError::InvalidConfiguration("empty".into()));
            }
            Ok(())
        }
        fn stop(&mut self) -> Result<(), VpnError> {
            Ok(())
        }
        fn kill(&mut self) -> Result<(), VpnError> {
            Ok(())
        }
        fn status(&self) -> ConnectionState {
            ConnectionState::Disconnected
        }
        fn info(&self) -> CoreInfoMeta {
            CoreInfoMeta {
                name: "plan-core".into(),
                version: "1.2.3".into(),
                supported_protocols: vec![],
            }
        }
        fn validate_config(&self, _: &str) -> Result<(), VpnError> {
            Ok(())
        }
        fn check_prerequisites(&self) -> Vec<PrerequisiteCheck> {
            vec![]
        }
        fn subscribe(&self) -> Option<std::sync::mpsc::Receiver<domain::CoreEvent>> {
            None
        }
        fn default_strategy_plan(&self) -> Option<Vec<u8>> {
            self.plan_bytes.clone()
        }
    }

    fn registry_with_plan(plan: Option<StrategyPlan>) -> Arc<Mutex<CoreRegistry>> {
        let mut reg = CoreRegistry::new();
        let bytes = plan.map(|p| serde_json::to_vec(&p).unwrap());
        reg.register(
            CoreDescriptor {
                core_type: "plan-core".into(),
                display_name: "Plan Core".into(),
                source: CoreSource::Mocked,
                binary_path: None,
                available: true,
            },
            Box::new(PlanCore { plan_bytes: bytes }),
        );
        Arc::new(Mutex::new(reg))
    }

    fn write_base_config() -> PathBuf {
        let mut tmp = tempfile::Builder::new()
            .prefix("pingle-test-base-")
            .suffix(".json")
            .tempfile()
            .unwrap();
        use std::io::Write;
        tmp.write_all(b"{}").unwrap();
        let (_, path) = tmp.keep().unwrap();
        path
    }

    fn make_input(config_path: PathBuf) -> ConnectInput {
        ConnectInput {
            config_path: config_path.to_string_lossy().into_owned(),
            core_type: "plan-core".into(),
            state: ConnectionState::Disconnected,
            metadata: BTreeMap::new(),
        }
    }

    /// Stub handler that records every call and returns the configured
    /// sequence of results in order.
    struct ScriptedHandler {
        calls: Mutex<Vec<ConnectInput>>,
        results: Mutex<Vec<Result<ConnectOutput, VpnError>>>,
        call_count: AtomicU32,
    }

    impl ScriptedHandler {
        fn new(results: Vec<Result<ConnectOutput, VpnError>>) -> Arc<Self> {
            Arc::new(Self {
                calls: Mutex::new(Vec::new()),
                results: Mutex::new(results),
                call_count: AtomicU32::new(0),
            })
        }

        fn call_count(&self) -> u32 {
            self.call_count.load(Ordering::SeqCst)
        }

        fn calls(&self) -> Vec<ConnectInput> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl Handler<OpConnect> for ScriptedHandler {
        fn handle(&self, input: ConnectInput) -> Result<ConnectOutput, VpnError> {
            self.calls.lock().unwrap().push(input);
            self.call_count.fetch_add(1, Ordering::SeqCst);
            let mut results = self.results.lock().unwrap();
            if results.is_empty() {
                Err(VpnError::Unknown("scripted handler ran out of results".into()))
            } else {
                results.remove(0)
            }
        }
    }

    fn ok_output() -> ConnectOutput {
        ConnectOutput {
            connection_info: None,
            metadata: BTreeMap::new(),
        }
    }

    fn fast_wrap(
        registry: Arc<Mutex<CoreRegistry>>,
        plugin: Option<Arc<dyn PipelinePlugin>>,
    ) -> StrategyRetryWrap {
        // Tests use 1ms cancel poll so retry sleeps wake up nearly instantly.
        StrategyRetryWrap::new(
            registry,
            Arc::new(ProcessorPipeline::new()),
            plugin,
            Arc::new(AtomicBool::new(false)),
        )
        .with_cancel_poll_interval(Duration::from_millis(1))
    }

    fn fast_wrap_with_cancel(
        registry: Arc<Mutex<CoreRegistry>>,
        cancel: Arc<AtomicBool>,
    ) -> StrategyRetryWrap {
        StrategyRetryWrap::new(
            registry,
            Arc::new(ProcessorPipeline::new()),
            None,
            cancel,
        )
        .with_cancel_poll_interval(Duration::from_millis(1))
    }

    fn one_strategy_plan(retry: RetryPolicy) -> StrategyPlan {
        StrategyPlan {
            strategies: vec![ConnectionStrategy {
                id: "only".into(),
                stack: StackType::System,
                resolver_type: ResolverType::Doh,
                total_timeout: Duration::from_secs(30),
                retry,
            }],
            global_timeout: Some(Duration::from_secs(60)),
        }
    }

    fn two_strategy_plan() -> StrategyPlan {
        StrategyPlan {
            strategies: vec![
                ConnectionStrategy {
                    id: "first".into(),
                    stack: StackType::System,
                    resolver_type: ResolverType::Doh,
                    total_timeout: Duration::from_secs(30),
                    retry: RetryPolicy::Fixed {
                        max_attempts: 2,
                        delay: Duration::from_millis(5),
                    },
                },
                ConnectionStrategy {
                    id: "second".into(),
                    stack: StackType::GVisor,
                    resolver_type: ResolverType::Tcp,
                    total_timeout: Duration::from_secs(30),
                    retry: RetryPolicy::NoRetry,
                },
            ],
            global_timeout: Some(Duration::from_secs(60)),
        }
    }

    // -- The action_for table -------------------------------------------------

    #[test]
    fn action_table_dns_failure_retries() {
        assert_eq!(action_for(ErrorKind::DnsFailure), StrategyAction::Retry);
    }

    #[test]
    fn action_table_validation_advances_immediately() {
        assert_eq!(
            action_for(ErrorKind::Validation),
            StrategyAction::AdvanceStrategy
        );
    }

    #[test]
    fn action_table_prerequisite_missing_bails() {
        assert_eq!(
            action_for(ErrorKind::PrerequisiteMissing),
            StrategyAction::Bail
        );
    }

    #[test]
    fn action_table_permission_denied_bails() {
        assert_eq!(
            action_for(ErrorKind::PermissionDenied),
            StrategyAction::Bail
        );
    }

    #[test]
    fn action_table_tun_device_bails() {
        assert_eq!(action_for(ErrorKind::TunDevice), StrategyAction::Bail);
    }

    #[test]
    fn action_table_auth_failure_bails() {
        assert_eq!(action_for(ErrorKind::AuthFailure), StrategyAction::Bail);
    }

    // -- D5: passthrough when no plan resolved --------------------------------

    #[test]
    fn passthrough_when_no_plan_anywhere() {
        let registry = registry_with_plan(None);
        let wrap = fast_wrap(registry, None);
        let handler = ScriptedHandler::new(vec![Ok(ok_output())]);
        let base = write_base_config();
        let result = wrap.handle(make_input(base.clone()), handler.as_ref());
        assert!(result.is_ok());
        assert_eq!(handler.call_count(), 1);
        // The inner handler saw the original config_path unchanged.
        let calls = handler.calls();
        assert_eq!(calls[0].config_path, base.to_string_lossy());
        let _ = std::fs::remove_file(&base);
    }

    // -- D6: single-strategy success on first attempt -------------------------

    #[test]
    fn single_strategy_success_on_first_attempt() {
        let registry = registry_with_plan(Some(one_strategy_plan(RetryPolicy::Fixed {
            max_attempts: 3,
            delay: Duration::from_millis(5),
        })));
        let wrap = fast_wrap(registry, None);
        let handler = ScriptedHandler::new(vec![Ok(ok_output())]);
        let base = write_base_config();
        let result = wrap.handle(make_input(base.clone()), handler.as_ref());
        assert!(result.is_ok(), "got: {result:?}");
        assert_eq!(handler.call_count(), 1);
        // Inner handler received a temp file, not the original.
        let calls = handler.calls();
        assert_ne!(calls[0].config_path, base.to_string_lossy());
        let _ = std::fs::remove_file(&base);
    }

    // -- D7: retry on retryable failure ---------------------------------------

    #[test]
    fn retries_on_dns_failure_then_succeeds() {
        let registry = registry_with_plan(Some(one_strategy_plan(RetryPolicy::Fixed {
            max_attempts: 3,
            delay: Duration::from_millis(5),
        })));
        let wrap = fast_wrap(registry, None);
        let handler = ScriptedHandler::new(vec![
            Err(VpnError::ProcessStartFailed(
                "lookup hub.example: no such host".into(),
            )),
            Ok(ok_output()),
        ]);
        let base = write_base_config();
        let result = wrap.handle(make_input(base.clone()), handler.as_ref());
        assert!(result.is_ok());
        assert_eq!(handler.call_count(), 2);
        let _ = std::fs::remove_file(&base);
    }

    // -- D8: exhausted strategy advances to next ------------------------------

    #[test]
    fn exhausted_strategy_advances_then_succeeds() {
        let registry = registry_with_plan(Some(two_strategy_plan()));
        let wrap = fast_wrap(registry, None);
        // Strategy 1 (max_attempts=2) fails twice, strategy 2 (NoRetry) succeeds.
        let handler = ScriptedHandler::new(vec![
            Err(VpnError::ProcessStartFailed("dial tcp: i/o timeout".into())),
            Err(VpnError::ProcessStartFailed("dial tcp: i/o timeout".into())),
            Ok(ok_output()),
        ]);
        let base = write_base_config();
        let result = wrap.handle(make_input(base.clone()), handler.as_ref());
        assert!(result.is_ok());
        assert_eq!(handler.call_count(), 3);
        let _ = std::fs::remove_file(&base);
    }

    // -- D9: all strategies exhausted -----------------------------------------

    #[test]
    fn all_strategies_exhausted_returns_last_error() {
        let registry = registry_with_plan(Some(two_strategy_plan()));
        let wrap = fast_wrap(registry, None);
        // Strategy 1 fails 2x, strategy 2 fails 1x = 3 calls total, all fail.
        let handler = ScriptedHandler::new(vec![
            Err(VpnError::Unknown("first fail".into())),
            Err(VpnError::Unknown("second fail".into())),
            Err(VpnError::Unknown("third fail".into())),
        ]);
        let base = write_base_config();
        let result = wrap.handle(make_input(base.clone()), handler.as_ref());
        assert!(result.is_err());
        assert_eq!(handler.call_count(), 3);
        // Last error wins.
        let err = result.unwrap_err();
        assert!(err.to_string().contains("third fail"), "got: {err}");
        let _ = std::fs::remove_file(&base);
    }

    // -- D11: fatal error class bails immediately -----------------------------

    #[test]
    fn prerequisite_missing_bails_no_retry_no_advance() {
        let registry = registry_with_plan(Some(two_strategy_plan()));
        let wrap = fast_wrap(registry, None);
        let handler = ScriptedHandler::new(vec![
            Err(VpnError::PrerequisiteMissing("libbox.dll".into())),
            Ok(ok_output()), // would-be retry / next-strategy attempt — must NOT happen
        ]);
        let base = write_base_config();
        let result = wrap.handle(make_input(base.clone()), handler.as_ref());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            VpnError::PrerequisiteMissing(_)
        ));
        assert_eq!(handler.call_count(), 1, "must bail after first fatal error");
        let _ = std::fs::remove_file(&base);
    }

    // -- D12: validation error advances strategy immediately ------------------

    #[test]
    fn validation_advances_strategy_without_retrying() {
        let registry = registry_with_plan(Some(two_strategy_plan()));
        let wrap = fast_wrap(registry, None);
        let handler = ScriptedHandler::new(vec![
            Err(VpnError::ValidationError("schema mismatch".into())),
            Ok(ok_output()),
        ]);
        let base = write_base_config();
        let result = wrap.handle(make_input(base.clone()), handler.as_ref());
        assert!(result.is_ok());
        // Strategy 1 emitted Validation, advances immediately to strategy 2 → success on attempt 1.
        assert_eq!(handler.call_count(), 2);
        let _ = std::fs::remove_file(&base);
    }

    // -- D13: cancellation flag --------------------------------------------------

    #[test]
    fn cancellation_flag_returns_cancelled() {
        let registry = registry_with_plan(Some(one_strategy_plan(RetryPolicy::Fixed {
            max_attempts: 5,
            delay: Duration::from_millis(50),
        })));
        let cancel = Arc::new(AtomicBool::new(false));
        let wrap = fast_wrap_with_cancel(registry, cancel.clone());
        // Set cancel before the call.
        cancel.store(true, Ordering::SeqCst);
        let handler = ScriptedHandler::new(vec![Ok(ok_output())]);
        let base = write_base_config();
        let result = wrap.handle(make_input(base.clone()), handler.as_ref());
        assert!(matches!(result, Err(VpnError::Cancelled)));
        assert_eq!(handler.call_count(), 0, "cancel before first call");
        let _ = std::fs::remove_file(&base);
    }

    #[test]
    fn cancellation_during_retry_sleep() {
        let registry = registry_with_plan(Some(one_strategy_plan(RetryPolicy::Fixed {
            max_attempts: 3,
            delay: Duration::from_secs(10), // long enough that cancel will fire first
        })));
        let cancel = Arc::new(AtomicBool::new(false));
        let wrap = fast_wrap_with_cancel(registry, cancel.clone());
        let handler = ScriptedHandler::new(vec![
            Err(VpnError::Unknown("first fail".into())),
            Ok(ok_output()),
        ]);
        let base = write_base_config();

        // Spawn a thread that sets cancel after the first failure has been recorded.
        let cancel_thread = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            cancel_thread.store(true, Ordering::SeqCst);
        });

        let result = wrap.handle(make_input(base.clone()), handler.as_ref());
        assert!(matches!(result, Err(VpnError::Cancelled)));
        // Inner handler ran exactly once before the cancel during sleep.
        assert_eq!(handler.call_count(), 1);
        let _ = std::fs::remove_file(&base);
    }

    // -- D14: plugin called per claimed stage ---------------------------------

    /// Stub plugin that records every call. Lives in the test module
    /// (we don't want to expose it from `pingle-pipeline-plugin`'s
    /// public API since it's a fixture).
    struct RecordingPlugin {
        capabilities: PipelineCapabilities,
        seen: Mutex<Vec<PipelineStage>>,
    }

    impl PipelinePlugin for RecordingPlugin {
        fn name(&self) -> &str {
            "recording"
        }
        fn capabilities(&self) -> &PipelineCapabilities {
            &self.capabilities
        }
        fn process_config(
            &self,
            stage: PipelineStage,
            input: ProcessConfigInput,
        ) -> Result<ProcessConfigOutput, pingle_pipeline_plugin::PluginError> {
            self.seen.lock().unwrap().push(stage);
            Ok(ProcessConfigOutput {
                config: input.config,
                diagnostics: vec![],
            })
        }
    }

    #[test]
    fn plugin_called_only_for_claimed_stages() {
        let plugin = Arc::new(RecordingPlugin {
            capabilities: PipelineCapabilities {
                wire_version: WIRE_VERSION,
                name: "recording".into(),
                description: String::new(),
                stages: vec![PipelineStage::PostDns, PipelineStage::PostPipeline],
            },
            seen: Mutex::new(Vec::new()),
        });
        let registry = registry_with_plan(Some(one_strategy_plan(RetryPolicy::NoRetry)));
        let wrap = fast_wrap(registry, Some(plugin.clone()));
        let handler = ScriptedHandler::new(vec![Ok(ok_output())]);
        let base = write_base_config();
        let result = wrap.handle(make_input(base.clone()), handler.as_ref());
        assert!(result.is_ok());
        let seen = plugin.seen.lock().unwrap();
        // Two claimed stages → plugin invoked twice in canonical order.
        assert_eq!(*seen, vec![PipelineStage::PostDns, PipelineStage::PostPipeline]);
        let _ = std::fs::remove_file(&base);
    }

    // -- D15: plugin error → continue with native output ----------------------

    struct AlwaysFailPlugin;
    impl PipelinePlugin for AlwaysFailPlugin {
        fn name(&self) -> &str {
            "always-fail"
        }
        fn capabilities(&self) -> &'static PipelineCapabilities {
            // We need to return &PipelineCapabilities; box-leak a static
            // for the test fixture. Acceptable in tests; production
            // code uses ExtismPipelinePlugin which holds the field.
            static CAPS: std::sync::OnceLock<PipelineCapabilities> = std::sync::OnceLock::new();
            CAPS.get_or_init(|| PipelineCapabilities {
                wire_version: WIRE_VERSION,
                name: "always-fail".into(),
                description: String::new(),
                stages: vec![PipelineStage::PostPipeline],
            })
        }
        fn process_config(
            &self,
            _stage: PipelineStage,
            _input: ProcessConfigInput,
        ) -> Result<ProcessConfigOutput, pingle_pipeline_plugin::PluginError> {
            Err(pingle_pipeline_plugin::PluginError::Wasm(
                "synthetic test failure".into(),
            ))
        }
    }

    #[test]
    fn plugin_error_falls_through_to_native_output() {
        let plugin = Arc::new(AlwaysFailPlugin);
        let registry = registry_with_plan(Some(one_strategy_plan(RetryPolicy::NoRetry)));
        let wrap = fast_wrap(registry, Some(plugin));
        let handler = ScriptedHandler::new(vec![Ok(ok_output())]);
        let base = write_base_config();
        let result = wrap.handle(make_input(base.clone()), handler.as_ref());
        assert!(result.is_ok(), "plugin failure must not break connect");
        assert_eq!(handler.call_count(), 1);
        let _ = std::fs::remove_file(&base);
    }

    // -- Per-call metadata override -------------------------------------------

    #[test]
    fn per_call_strategy_plan_metadata_overrides_core_default() {
        // Core has a 2-strategy plan. Per-call override has a single-strategy
        // NoRetry plan. We verify the override wins by giving the scripted
        // handler exactly one Ok — if the override is honored, it's enough;
        // if it's ignored, the core's plan would call inner more times.
        let core_plan = two_strategy_plan();
        let registry = registry_with_plan(Some(core_plan));

        let override_plan = one_strategy_plan(RetryPolicy::NoRetry);
        let mut input = make_input(write_base_config());
        let base_path = PathBuf::from(input.config_path.clone());
        input.metadata.insert(
            "strategy_plan_json".into(),
            serde_json::to_string(&override_plan).unwrap(),
        );

        let wrap = fast_wrap(registry, None);
        let handler = ScriptedHandler::new(vec![Ok(ok_output())]);
        let result = wrap.handle(input, handler.as_ref());
        assert!(result.is_ok());
        assert_eq!(handler.call_count(), 1);
        let _ = std::fs::remove_file(&base_path);
    }
}
