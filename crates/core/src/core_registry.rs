use log::info;
use pingling_domain::{CoreDescriptor, CoreSource, VpnCore, VpnError};
use std::collections::BTreeMap;

pub struct CoreRegistry {
    cores: BTreeMap<String, Box<dyn VpnCore>>,
    descriptors: BTreeMap<String, CoreDescriptor>,
    active: Option<String>,
}

impl CoreRegistry {
    pub fn new() -> Self {
        Self {
            cores: BTreeMap::new(),
            descriptors: BTreeMap::new(),
            active: None,
        }
    }

    pub fn register(&mut self, descriptor: CoreDescriptor, core: Box<dyn VpnCore>) {
        let key = descriptor.core_type.clone();
        if self.active.is_none() {
            self.active = Some(key.clone());
        }
        self.descriptors.insert(key.clone(), descriptor);
        self.cores.insert(key, core);
    }

    pub fn discover(&mut self) {
        self.discover_system_cores();
        if self.active.is_none() {
            self.active = self.cores.keys().next().cloned();
        }
    }

    fn discover_system_cores(&mut self) {
        let known_cores: &[(&str, &str)] = &[("sing-box", "Sing-Box"), ("xray", "Xray")];
        for (core_type, display_name) in known_cores {
            if self.descriptors.contains_key(*core_type) {
                continue;
            }
            if let Some(path) = pingling_util::which(core_type) {
                let available = std::path::Path::new(&path).exists();
                let descriptor = CoreDescriptor {
                    core_type: core_type.to_string(),
                    display_name: display_name.to_string(),
                    source: CoreSource::System,
                    binary_path: Some(path),
                    available,
                };
                if available {
                    info!("discovered system core: {core_type}");
                }
                self.descriptors.insert(core_type.to_string(), descriptor);
            }
        }
    }

    pub fn list(&self) -> Vec<&CoreDescriptor> {
        self.descriptors.values().collect()
    }

    pub fn descriptor(&self, core_type: &str) -> Option<&CoreDescriptor> {
        self.descriptors.get(core_type)
    }

    pub fn active_type(&self) -> Option<&str> {
        self.active.as_deref()
    }

    pub fn active_core(&mut self) -> Option<&mut Box<dyn VpnCore>> {
        let key = self.active.as_ref()?;
        self.cores.get_mut(key.as_str())
    }

    pub fn get_core(&mut self, core_type: &str) -> Option<&mut Box<dyn VpnCore>> {
        self.cores.get_mut(core_type)
    }

    pub fn switch(&mut self, core_type: &str) -> Result<(), VpnError> {
        let desc = self
            .descriptors
            .get(core_type)
            .ok_or_else(|| VpnError::CoreNotFound(core_type.to_string()))?;
        if !desc.available {
            return Err(VpnError::PrerequisiteMissing(format!(
                "core '{}' is not available",
                core_type
            )));
        }
        if !self.cores.contains_key(core_type) {
            return Err(VpnError::CoreNotFound(format!(
                "{core_type} (no registered instance)"
            )));
        }
        self.active = Some(core_type.to_string());
        info!("switched active core to: {core_type}");
        Ok(())
    }

    pub fn set_binary_path(&mut self, core_type: &str, path: &str) -> Result<(), VpnError> {
        let desc = self
            .descriptors
            .get_mut(core_type)
            .ok_or_else(|| VpnError::CoreNotFound(core_type.to_string()))?;
        desc.binary_path = Some(path.to_string());
        desc.available = std::path::Path::new(path).exists();
        desc.source = CoreSource::Linked(path.to_string());
        info!("updated {core_type} binary path: {path}");
        Ok(())
    }
}

impl Default for CoreRegistry {
    fn default() -> Self {
        Self::new()
    }
}
