use pingling_config_pipeline::strategy::{
    ConnectionStrategy, ResolverType, RetryPolicy, StackType, StrategyPlan,
};
use pingling_core_process::{ProcessCore, ProcessCoreSpec};
use pingling_domain::{ConnectionState, CoreEvent, CoreInfo, PrerequisiteCheck, VpnCore, VpnError};
use std::sync::mpsc;
use std::time::Duration;

pub struct SingboxCore {
    inner: ProcessCore,
}

impl SingboxCore {
    pub fn new(binary_path: &str) -> Self {
        let binary = if binary_path.is_empty() {
            "sing-box"
        } else {
            binary_path
        };
        Self {
            inner: ProcessCore::new(singbox_process_spec(binary)),
        }
    }
}

pub fn singbox_process_spec(binary: impl Into<String>) -> ProcessCoreSpec {
    ProcessCoreSpec::new("sing-box", binary)
        .with_display_name("sing-box")
        .with_version("standalone")
        .with_start_args(["run", "-c", "{config}"])
        .with_validate_args(["check", "-c", "{config}"])
        .with_supported_protocols([
            "vmess",
            "vless",
            "trojan",
            "shadowsocks",
            "wireguard",
            "hysteria",
            "hysteria2",
            "tuic",
        ])
}

impl VpnCore for SingboxCore {
    fn start(&mut self, config_path: &str) -> Result<(), VpnError> {
        self.inner.start(config_path)
    }

    fn stop(&mut self) -> Result<(), VpnError> {
        self.inner.stop()
    }

    fn kill(&mut self) -> Result<(), VpnError> {
        self.inner.kill()
    }

    fn status(&self) -> ConnectionState {
        self.inner.status()
    }

    fn running(&self) -> bool {
        self.inner.running()
    }

    fn info(&self) -> CoreInfo {
        self.inner.info()
    }

    fn validate_config(&self, config_path: &str) -> Result<(), VpnError> {
        self.inner.validate_config(config_path)
    }

    fn check_prerequisites(&self) -> Vec<PrerequisiteCheck> {
        self.inner.check_prerequisites()
    }

    fn subscribe(&self) -> Option<mpsc::Receiver<CoreEvent>> {
        self.inner.subscribe()
    }

    fn default_strategy_plan(&self) -> Option<Vec<u8>> {
        Some(default_singbox_standalone_strategy_plan_json())
    }
}

pub fn default_singbox_standalone_strategy_plan_json() -> Vec<u8> {
    let plan = StrategyPlan {
        strategies: vec![
            ConnectionStrategy {
                id: "default-doh".into(),
                stack: StackType::System,
                resolver_type: ResolverType::Doh,
                total_timeout: Duration::from_secs(25),
                retry: RetryPolicy::Fixed {
                    max_attempts: 3,
                    delay: Duration::from_secs(2),
                },
            },
            ConnectionStrategy {
                id: "fallback-tcp".into(),
                stack: StackType::System,
                resolver_type: ResolverType::Tcp,
                total_timeout: Duration::from_secs(25),
                retry: RetryPolicy::Fixed {
                    max_attempts: 2,
                    delay: Duration::from_secs(3),
                },
            },
        ],
        global_timeout: Some(Duration::from_secs(60)),
    };
    serde_json::to_vec(&plan).expect("default singbox strategy plan must serialize")
}

#[cfg(test)]
mod tests {
    use super::*;
    use pingling_config_pipeline::strategy::{ResolverType, StackType};

    #[test]
    fn preset_uses_singbox_cli_contract() {
        let spec = singbox_process_spec("sing-box");
        assert_eq!(spec.start_args, vec!["run", "-c", "{config}"]);
        assert_eq!(spec.validate_args, vec!["check", "-c", "{config}"]);
    }

    #[test]
    fn strategy_plan_round_trips() {
        let bytes = default_singbox_standalone_strategy_plan_json();
        let plan: StrategyPlan = serde_json::from_slice(&bytes).expect("valid json");
        assert_eq!(plan.strategies.len(), 2);
        assert_eq!(plan.strategies[0].id, "default-doh");
        assert_eq!(plan.strategies[0].stack, StackType::System);
        assert_eq!(plan.strategies[0].resolver_type, ResolverType::Doh);
        assert_eq!(plan.strategies[1].id, "fallback-tcp");
        assert_eq!(plan.global_timeout.unwrap().as_secs(), 60);
    }
}
