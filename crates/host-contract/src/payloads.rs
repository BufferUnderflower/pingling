use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const VPN_CONNECT_WIRE_VERSION: u32 = 1;
pub const VPN_DISCONNECT_WIRE_VERSION: u32 = 1;
pub const IPC_DISPATCH_WIRE_VERSION: u32 = 1;
pub const CORE_START_WIRE_VERSION: u32 = 1;
pub const CORE_STOP_WIRE_VERSION: u32 = 1;
pub const PROFILE_ACTIVATE_WIRE_VERSION: u32 = 1;
pub const PROFILE_PERSIST_WIRE_VERSION: u32 = 1;
pub const DAEMON_STARTUP_WIRE_VERSION: u32 = 1;
pub const DAEMON_SHUTDOWN_WIRE_VERSION: u32 = 1;
pub const OUTBOUND_SELECT_WIRE_VERSION: u32 = 1;
pub const OUTBOUND_TEST_LATENCY_WIRE_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VpnConnectPayload {
    pub core_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<ConnectResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectResult {
    pub started: bool,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VpnDisconnectPayload {
    pub core_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<DisconnectResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisconnectResult {
    pub stopped: bool,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcDispatchPayload {
    pub method: String,
    pub params: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<IpcDispatchOutcome>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcDispatchOutcome {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    pub duration_us: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreStartPayload {
    pub core_type: String,
    pub config_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<CoreLifecycleResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreStopPayload {
    pub core_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<CoreLifecycleResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreLifecycleResult {
    pub ok: bool,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileActivatePayload {
    pub profile_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfilePersistPayload {
    pub profile_id: String,
    pub profile: Value,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonStartupPayload {
    pub version: String,
    pub plugin_count: u32,
    pub core_types: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonShutdownPayload {
    pub trigger: String,
    pub uptime_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboundSelectPayload {
    pub tag: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboundTestLatencyPayload {
    pub tag: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<LatencyResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatencyResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rtt_ms: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigProcessPayload {
    pub config: Value,
    #[serde(default)]
    pub request: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeeplinkResolvePayload {
    pub request: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthSessionPayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginLoadPayload {
    #[serde(default)]
    pub plugin_id: String,
    #[serde(default)]
    pub manifest: Option<Value>,
}

pub mod config {
    use crate::{HostFailure, HostResult};
    use serde_json::Value;

    #[derive(Debug, Clone)]
    pub struct ProcessorStep {
        pub processor_name: String,
        pub input: Value,
        pub output: Value,
    }

    pub trait ConfigProcessor: Send + Sync {
        fn name(&self) -> &str;
        fn process(&self, config: Value, request: &Value) -> HostResult<Value>;
    }

    #[derive(Default)]
    pub struct ProcessorPipeline {
        processors: Vec<Box<dyn ConfigProcessor>>,
    }

    impl ProcessorPipeline {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn push(&mut self, processor: Box<dyn ConfigProcessor>) -> &mut Self {
            self.processors.push(processor);
            self
        }

        pub fn names(&self) -> Vec<&str> {
            self.processors
                .iter()
                .map(|processor| processor.name())
                .collect()
        }

        pub fn is_empty(&self) -> bool {
            self.processors.is_empty()
        }

        pub fn process(&self, config: Value, request: &Value) -> HostResult<Value> {
            self.process_with(&mut |_| {}, config, request)
        }

        pub fn process_with(
            &self,
            on_step: &mut dyn FnMut(ProcessorStep),
            mut config: Value,
            request: &Value,
        ) -> HostResult<Value> {
            for processor in &self.processors {
                let input = config.clone();
                let output = processor.process(config, request).map_err(|error| {
                    HostFailure::plugin_error(format!("processor[{}]: {error}", processor.name()))
                })?;
                on_step(ProcessorStep {
                    processor_name: processor.name().to_owned(),
                    input,
                    output: output.clone(),
                });
                config = output;
            }
            Ok(config)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn round_trip<T>(value: T)
    where
        T: Serialize + for<'de> Deserialize<'de>,
    {
        let json_str = serde_json::to_string(&value).expect("serialize");
        let parsed: T = serde_json::from_str(&json_str).expect("deserialize");
        let reserialized = serde_json::to_string(&parsed).expect("reserialize");
        assert_eq!(json_str, reserialized);
    }

    #[test]
    fn wired_payloads_round_trip() {
        round_trip(VpnConnectPayload {
            core_type: "sing-box".into(),
            config_path: Some("/tmp/config.json".into()),
            hint: Some(json!({"attempt": 2})),
            result: Some(ConnectResult {
                started: true,
                duration_ms: 842,
                error: None,
            }),
        });
        round_trip(VpnDisconnectPayload {
            core_type: "sing-box".into(),
            reason: Some("user".into()),
            result: Some(DisconnectResult {
                stopped: true,
                duration_ms: 120,
                error: None,
            }),
        });
        round_trip(IpcDispatchPayload {
            method: "vpn.status".into(),
            params: json!({}),
            transport: Some("uds".into()),
            outcome: Some(IpcDispatchOutcome {
                ok: true,
                error_code: None,
                error_message: None,
                duration_us: 450,
            }),
        });
    }

    #[test]
    fn scaffolded_payloads_round_trip() {
        round_trip(CoreStartPayload {
            core_type: "sing-box".into(),
            config_path: "/tmp/c.json".into(),
            result: None,
        });
        round_trip(ProfilePersistPayload {
            profile_id: "p-1".into(),
            profile: json!({"core_type": "sing-box"}),
            source: "deeplink".into(),
        });
        round_trip(DaemonStartupPayload {
            version: "0.1.3".into(),
            plugin_count: 1,
            core_types: vec!["sing-box".into(), "mock".into()],
        });
        round_trip(OutboundTestLatencyPayload {
            tag: "ss-jp".into(),
            result: Some(LatencyResult {
                rtt_ms: Some(42),
                error: None,
            }),
        });
    }

    #[test]
    fn config_pipeline_runs_in_order() {
        struct AppendKey(&'static str);

        impl config::ConfigProcessor for AppendKey {
            fn name(&self) -> &str {
                self.0
            }

            fn process(&self, mut config: Value, _request: &Value) -> crate::HostResult<Value> {
                config
                    .as_object_mut()
                    .expect("test input is object")
                    .insert(self.0.into(), json!(true));
                Ok(config)
            }
        }

        let mut pipeline = config::ProcessorPipeline::new();
        pipeline
            .push(Box::new(AppendKey("first")))
            .push(Box::new(AppendKey("second")));

        let output = pipeline.process(json!({}), &json!({})).unwrap();

        assert_eq!(output["first"], json!(true));
        assert_eq!(output["second"], json!(true));
    }
}
