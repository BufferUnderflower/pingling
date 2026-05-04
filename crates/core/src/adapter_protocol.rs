use pingling_domain::VpnError;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PINGLING_CORE_ABI_VERSION: u32 = 1;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct AdapterManifest {
    pub schema: u32,
    pub abi: u32,
    pub core_type: String,
    pub engine: String,
    pub version: String,
    pub display_name: String,
    pub library: String,
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub supported_protocols: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterPackage {
    pub manifest: AdapterManifest,
    pub package_dir: std::path::PathBuf,
    pub library_path: std::path::PathBuf,
}

pub trait AdapterClient: Send {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, VpnError>;
}

pub fn validate_adapter_manifest(
    manifest: &AdapterManifest,
    path: &std::path::Path,
) -> Result<(), VpnError> {
    if manifest.schema != 1 {
        return Err(VpnError::InvalidConfiguration(format!(
            "{} uses unsupported schema {}",
            path.display(),
            manifest.schema
        )));
    }
    if manifest.abi != PINGLING_CORE_ABI_VERSION {
        return Err(VpnError::InvalidConfiguration(format!(
            "{} requires unsupported ABI {}",
            path.display(),
            manifest.abi
        )));
    }
    if manifest.core_type.trim().is_empty()
        || manifest.engine.trim().is_empty()
        || manifest.version.trim().is_empty()
        || manifest.library.trim().is_empty()
    {
        return Err(VpnError::InvalidConfiguration(format!(
            "{} is missing required adapter identity fields",
            path.display()
        )));
    }
    validate_library_path(&manifest.library, path)?;
    Ok(())
}

fn validate_library_path(library: &str, path: &std::path::Path) -> Result<(), VpnError> {
    use std::path::Component;
    let library_path = std::path::Path::new(library);
    let stays_inside_package = library_path
        .components()
        .all(|component| matches!(component, Component::Normal(_) | Component::CurDir));
    if library_path.is_absolute() || !stays_inside_package {
        return Err(VpnError::InvalidConfiguration(format!(
            "{} uses unsafe adapter library path {}",
            path.display(),
            library
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_roundtrip() {
        let manifest = AdapterManifest {
            schema: 1,
            abi: PINGLING_CORE_ABI_VERSION,
            core_type: "test@1.0".into(),
            engine: "test".into(),
            version: "1.0.0".into(),
            display_name: "Test".into(),
            library: "test.dll".into(),
            priority: 100,
            supported_protocols: vec!["vless".into()],
        };
        let json = serde_json::to_string(&manifest).unwrap();
        let parsed: AdapterManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(manifest, parsed);
    }

    #[test]
    fn rejects_unsafe_library_path() {
        let manifest = AdapterManifest {
            schema: 1,
            abi: PINGLING_CORE_ABI_VERSION,
            core_type: "test".into(),
            engine: "test".into(),
            version: "1.0".into(),
            display_name: "Test".into(),
            library: "../escape.dll".into(),
            priority: 1,
            supported_protocols: vec![],
        };
        let path = std::path::Path::new("/tmp/pkg/manifest.json");
        let err = validate_adapter_manifest(&manifest, path).unwrap_err();
        assert!(err.to_string().contains("unsafe adapter library path"));
    }
}
