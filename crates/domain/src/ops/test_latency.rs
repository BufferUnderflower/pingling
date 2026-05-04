//! Latency testing — a capability operation.

use crate::pipeline::Operation;
use std::collections::BTreeMap;

/// Test latency to outbounds. Results are `outbound_id → latency_ms`.
pub struct OpTestLatency;

#[derive(Debug, Clone)]
pub struct TestLatencyInput {
    /// Outbound IDs to test. Empty = test all.
    pub outbound_ids: Vec<String>,
    pub core_type: String,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct TestLatencyOutput {
    /// Measured latencies. Missing entries = timeout / unreachable.
    pub results: BTreeMap<String, u32>,
    pub metadata: BTreeMap<String, String>,
}

impl Operation for OpTestLatency {
    type Input = TestLatencyInput;
    type Output = TestLatencyOutput;
    fn name() -> &'static str {
        "test_latency"
    }
}
