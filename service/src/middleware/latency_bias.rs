//! Latency bias hook — adjusts latency scores by geographic preference.
//!
//! [`LatencyBiasHook`] implements two hooks that work in tandem:
//!
//! - `Hook<OpListOutbounds>` — `after` phase: reads the outbound list
//!   produced by the handler and **caches** the `outbound_id → country_code`
//!   mapping internally. The list is returned unmodified.
//!
//! - `Hook<OpTestLatency>` — `after` phase: reads the latency results
//!   produced by the handler and adds a penalty (in ms) to outbounds from
//!   non-preferred countries.
//!
//! The shared state is a `Mutex<BTreeMap<…>>` so both hooks can live in one
//! struct and share it without locking the two pipelines against each other.
//!
//! # Usage
//!
//! ```rust,ignore
//! use service::middleware::latency_bias::LatencyBiasHook;
//! use std::sync::Arc;
//!
//! let bias = Arc::new(LatencyBiasHook::new(vec!["JP".into(), "SG".into()], 50));
//! list_outbounds_pipeline.push_hook(Box::new(Arc::clone(&bias)));
//! test_latency_pipeline.push_hook(Box::new(Arc::clone(&bias)));
//! ```
//!
//! Note the `Arc` sharing: both pipelines hold a reference to the same hook
//! so the country-cache written by the list-outbounds pass is visible to the
//! test-latency pass.
//!
//! # State lifecycle
//!
//! Country codes are learned lazily on the first list-outbounds call and
//! retained across subsequent latency tests. If outbounds change (new server
//! added), the cache is updated on the next list-outbounds call.

use domain::ops::*;
use domain::pipeline::Hook;
use domain::VpnError;
use std::collections::BTreeMap;
use std::sync::Mutex;

/// Adds a latency penalty (ms) to outbounds from non-preferred countries.
///
/// Learns country codes by observing the `OpListOutbounds` output, then
/// applies a geographic bias when `OpTestLatency` results are produced.
pub struct LatencyBiasHook {
    /// ISO 3166-1 alpha-2 codes whose outbounds receive *no* penalty.
    preferred: Vec<String>,
    /// Penalty added to latency results for non-preferred outbounds, in ms.
    penalty_ms: u32,
    /// Learned from the list-outbounds pass: outbound_id → country_code.
    country_cache: Mutex<BTreeMap<String, String>>,
}

impl LatencyBiasHook {
    pub fn new(preferred_countries: Vec<String>, penalty_ms: u32) -> Self {
        Self {
            preferred: preferred_countries,
            penalty_ms,
            country_cache: Mutex::new(BTreeMap::new()),
        }
    }
}

// -- Observe the outbound list; update the country cache ---------------------

impl Hook<OpListOutbounds> for LatencyBiasHook {
    fn name(&self) -> &str {
        "example:latency-bias"
    }

    /// After the list is produced, populate `outbound_id → country_code` cache.
    ///
    /// The output is returned unchanged; this is a pure observation hook.
    fn after(
        &self,
        _input: &ListOutboundsInput,
        output: &mut ListOutboundsOutput,
    ) -> Result<(), VpnError> {
        let mut cache = self.country_cache.lock().unwrap_or_else(|e| e.into_inner());
        for o in &output.outbounds {
            if let Some(ref cc) = o.country_code {
                cache.insert(o.id.clone(), cc.clone());
            }
        }
        Ok(())
    }
}

// -- Apply the bias to latency results ---------------------------------------

impl Hook<OpTestLatency> for LatencyBiasHook {
    fn name(&self) -> &str {
        "example:latency-bias"
    }

    /// After latency results are produced, add `penalty_ms` to non-preferred
    /// outbounds. Records how many were penalized in output metadata.
    fn after(
        &self,
        _input: &TestLatencyInput,
        output: &mut TestLatencyOutput,
    ) -> Result<(), VpnError> {
        let cache = self.country_cache.lock().unwrap_or_else(|e| e.into_inner());

        let mut penalized = 0u32;
        for (id, latency) in output.results.iter_mut() {
            if let Some(cc) = cache.get(id) {
                if !self.preferred.contains(cc) {
                    *latency = latency.saturating_add(self.penalty_ms);
                    penalized += 1;
                }
            }
        }

        if penalized > 0 {
            output
                .metadata
                .insert("latency-bias:penalized".into(), penalized.to_string());
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use domain::pipeline::{Handler, Pipeline};
    use domain::types::{Outbound, OutboundProtocol, OutboundTransport};
    use std::sync::Arc;

    fn make_outbound(id: &str, cc: &str) -> Outbound {
        Outbound {
            id: id.into(),
            name: id.into(),
            protocol: OutboundProtocol::Vless,
            transport: OutboundTransport::Tcp,
            country_code: Some(cc.into()),
            location: None,
            latency_ms: None,
            selected: false,
            metadata: Default::default(),
        }
    }

    struct OutboundHandler(Vec<Outbound>);
    impl Handler<OpListOutbounds> for OutboundHandler {
        fn handle(&self, _: ListOutboundsInput) -> Result<ListOutboundsOutput, VpnError> {
            Ok(ListOutboundsOutput {
                outbounds: self.0.clone(),
                metadata: Default::default(),
            })
        }
    }

    struct LatencyHandler(BTreeMap<String, u32>);
    impl Handler<OpTestLatency> for LatencyHandler {
        fn handle(&self, _: TestLatencyInput) -> Result<TestLatencyOutput, VpnError> {
            Ok(TestLatencyOutput {
                results: self.0.clone(),
                metadata: Default::default(),
            })
        }
    }

    fn list_input() -> ListOutboundsInput {
        ListOutboundsInput {
            core_type: "mock".into(),
            config_path: None,
            metadata: Default::default(),
        }
    }

    fn latency_input() -> TestLatencyInput {
        TestLatencyInput {
            outbound_ids: vec![],
            core_type: "mock".into(),
            metadata: Default::default(),
        }
    }

    // -- Core scenario: learn countries then apply bias -----------------------

    #[test]
    fn learns_countries_then_biases_latency() {
        let bias = Arc::new(LatencyBiasHook::new(vec!["JP".into()], 50));

        // Step 1: list outbounds → learn country codes
        let mut list_p = Pipeline::new(Box::new(OutboundHandler(vec![
            make_outbound("jp-1", "JP"),
            make_outbound("us-1", "US"),
            make_outbound("de-1", "DE"),
        ])));
        list_p.push_hook(Box::new(Arc::clone(&bias)));
        list_p.execute(list_input()).unwrap();

        // Step 2: test latency → apply bias
        let mut results = BTreeMap::new();
        results.insert("jp-1".into(), 30u32);
        results.insert("us-1".into(), 25u32);
        results.insert("de-1".into(), 20u32);

        let mut lat_p = Pipeline::new(Box::new(LatencyHandler(results)));
        lat_p.push_hook(Box::new(Arc::clone(&bias)));

        let output = lat_p.execute(latency_input()).unwrap();
        assert_eq!(output.results["jp-1"], 30); // preferred — no penalty
        assert_eq!(output.results["us-1"], 75); // 25 + 50
        assert_eq!(output.results["de-1"], 70); // 20 + 50
        assert_eq!(
            output
                .metadata
                .get("latency-bias:penalized")
                .map(|s| s.as_str()),
            Some("2")
        );
    }

    // -- List output is not modified -----------------------------------------

    #[test]
    fn list_outbounds_output_is_unmodified() {
        let bias = Arc::new(LatencyBiasHook::new(vec!["JP".into()], 100));

        let mut list_p = Pipeline::new(Box::new(OutboundHandler(vec![
            make_outbound("jp-1", "JP"),
            make_outbound("us-1", "US"),
        ])));
        list_p.push_hook(Box::new(Arc::clone(&bias)));

        let output = list_p.execute(list_input()).unwrap();
        // Hook must not remove or modify outbounds, only cache them.
        assert_eq!(output.outbounds.len(), 2);
        assert!(output.metadata.is_empty());
    }

    // -- No penalty for preferred country -------------------------------------

    #[test]
    fn preferred_country_receives_no_penalty() {
        let bias = Arc::new(LatencyBiasHook::new(vec!["JP".into(), "SG".into()], 100));

        let mut list_p = Pipeline::new(Box::new(OutboundHandler(vec![
            make_outbound("jp-1", "JP"),
            make_outbound("sg-1", "SG"),
        ])));
        list_p.push_hook(Box::new(Arc::clone(&bias)));
        list_p.execute(list_input()).unwrap();

        let mut results = BTreeMap::new();
        results.insert("jp-1".into(), 40u32);
        results.insert("sg-1".into(), 55u32);

        let mut lat_p = Pipeline::new(Box::new(LatencyHandler(results)));
        lat_p.push_hook(Box::new(Arc::clone(&bias)));

        let output = lat_p.execute(latency_input()).unwrap();
        assert_eq!(output.results["jp-1"], 40); // no penalty
        assert_eq!(output.results["sg-1"], 55); // no penalty
        assert!(!output.metadata.contains_key("latency-bias:penalized"));
    }

    // -- Saturating add on overflow ------------------------------------------

    #[test]
    fn saturating_add_prevents_overflow() {
        let bias = Arc::new(LatencyBiasHook::new(vec![], 1000)); // no preferred

        let mut list_p =
            Pipeline::new(Box::new(OutboundHandler(vec![make_outbound("us-1", "US")])));
        list_p.push_hook(Box::new(Arc::clone(&bias)));
        list_p.execute(list_input()).unwrap();

        let mut results = BTreeMap::new();
        results.insert("us-1".into(), u32::MAX);

        let mut lat_p = Pipeline::new(Box::new(LatencyHandler(results)));
        lat_p.push_hook(Box::new(Arc::clone(&bias)));

        let output = lat_p.execute(latency_input()).unwrap();
        assert_eq!(output.results["us-1"], u32::MAX); // saturated, no panic
    }

    // -- Unknown outbound ID in latency results (not in cache) ---------------

    #[test]
    fn unknown_outbound_id_not_penalized() {
        let bias = Arc::new(LatencyBiasHook::new(vec!["JP".into()], 50));

        // Never called list_outbounds → cache is empty
        let mut results = BTreeMap::new();
        results.insert("mystery-1".into(), 30u32);

        let mut lat_p = Pipeline::new(Box::new(LatencyHandler(results)));
        lat_p.push_hook(Box::new(Arc::clone(&bias)));

        let output = lat_p.execute(latency_input()).unwrap();
        // No cache entry → no penalty applied.
        assert_eq!(output.results["mystery-1"], 30);
        assert!(!output.metadata.contains_key("latency-bias:penalized"));
    }

    // -- Neither hook modifies on error path ---------------------------------

    #[test]
    fn on_error_is_noop() {
        struct FailLatencyHandler;
        impl Handler<OpTestLatency> for FailLatencyHandler {
            fn handle(&self, _: TestLatencyInput) -> Result<TestLatencyOutput, VpnError> {
                Err(VpnError::Unknown("latency probe failed".into()))
            }
        }

        let bias = LatencyBiasHook::new(vec!["JP".into()], 50);
        let mut lat_p = Pipeline::new(Box::new(FailLatencyHandler));
        lat_p.push_hook(Box::new(bias));

        // Error propagates unchanged — on_error default is noop.
        assert!(lat_p.execute(latency_input()).is_err());
    }
}
