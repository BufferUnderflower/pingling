use pingling_host_contract::{
    DiagnosticLevel, ExtensionHost, HostDiagnostic, HostInvocation, HostOutcome, HostResult,
};

#[derive(Clone, Debug, Default)]
pub struct PassthroughHost;

impl ExtensionHost for PassthroughHost {
    fn invoke(&self, invocation: HostInvocation) -> HostResult<HostOutcome> {
        Ok(HostOutcome {
            diagnostics: vec![HostDiagnostic {
                level: DiagnosticLevel::Debug,
                message: "no extension installed; payload returned unchanged".to_owned(),
            }],
            payload: invocation.payload,
        })
    }
}

pub fn passthrough(invocation: HostInvocation) -> HostResult<HostOutcome> {
    PassthroughHost.invoke(invocation)
}

#[cfg(test)]
mod tests {
    use pingling_host_contract::{ExtensionHost, HostInvocation, Slot};

    use super::*;

    #[test]
    fn returns_payload_unchanged() {
        let payload = serde_json::json!({"value": 42});
        let invocation = HostInvocation::new(
            Slot::new(Slot::CONFIG_TRANSFORM).unwrap(),
            "apply",
            payload.clone(),
        )
        .unwrap();

        let outcome = PassthroughHost.invoke(invocation).unwrap();

        assert_eq!(outcome.payload, payload);
        assert_eq!(outcome.diagnostics.len(), 1);
    }
}
