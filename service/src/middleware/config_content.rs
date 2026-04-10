//! Config content loader hook — reads the config file into the pipeline.
//!
//! [`ConfigContentLoader`] implements `Hook<OpValidateConfig>` using the
//! `before` phase. It reads the file at `input.config_path` and stores the
//! raw text in `input.config_content`.
//!
//! This enables downstream hooks — especially WASM plugins — to inspect and
//! transform the actual config content, not just the file path.
//!
//! # Why a dedicated hook
//!
//! File I/O should happen **once** at a well-defined point in the pipeline,
//! not scattered across multiple hooks. By centralizing the read here, every
//! subsequent `before` hook and the handler itself can rely on
//! `config_content` being populated.
//!
//! # Failure policy
//!
//! If the file cannot be read (not found, permission error, etc.), the hook
//! records a `config-content:error` entry in metadata and continues — it does
//! **not** abort the pipeline. The core's `validate_config` may still succeed
//! if it validates from `config_path` directly. Plugins that require content
//! should check `config_content.is_some()` and reject via their own `before`
//! if it is absent.
//!
//! # Registration order
//!
//! Register `ConfigContentLoader` **before** `ValidateBeforeStart` and before
//! any plugin hooks so that `config_content` is populated when they run.
//!
//! ```rust,ignore
//! use service::middleware::{config_content::ConfigContentLoader, validate::ValidateBeforeStart};
//!
//! // Registers hooks in order: ContentLoader → Validate → (plugins)
//! pipeline.push_hook(Box::new(ConfigContentLoader::new()));
//! pipeline.push_hook(Box::new(ValidateBeforeStart::new(registry)));
//! ```

#[cfg(test)]
use domain::ops::ValidateConfigOutput;
use domain::ops::{OpValidateConfig, ValidateConfigInput};
use domain::pipeline::Hook;
use domain::VpnError;
use log::warn;

/// Reads the config file and populates `ValidateConfigInput::config_content`.
///
/// Runs in the `before` phase so all subsequent hooks and the terminal handler
/// see the content. On read failure, records an error in metadata and
/// continues (non-aborting).
pub struct ConfigContentLoader;

impl ConfigContentLoader {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ConfigContentLoader {
    fn default() -> Self {
        Self::new()
    }
}

impl Hook<OpValidateConfig> for ConfigContentLoader {
    fn name(&self) -> &str {
        "builtin:config-content-loader"
    }

    /// Reads `config_path` and stores the contents in `config_content`.
    ///
    /// On success, `config_content` is `Some(raw_text)`.
    /// On failure, `config_content` remains `None` and a warning is recorded
    /// in metadata under `"config-content:error"`.
    fn before(&self, input: &mut ValidateConfigInput) -> Result<(), VpnError> {
        match std::fs::read_to_string(&input.config_path) {
            Ok(content) => {
                input.config_content = Some(content);
            }
            Err(e) => {
                let msg = format!("could not read {}: {e}", input.config_path);
                warn!("[config-content-loader] {msg}");
                input.metadata.insert("config-content:error".into(), msg);
                // Non-fatal: let the handler attempt validation from path.
            }
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
    use domain::pipeline::{FnHook, Handler, Pipeline};
    use std::collections::BTreeMap;
    use std::io::Write as _;

    fn ok_handler() -> impl Handler<OpValidateConfig> {
        struct H;
        impl Handler<OpValidateConfig> for H {
            fn handle(&self, _: ValidateConfigInput) -> Result<ValidateConfigOutput, VpnError> {
                Ok(ValidateConfigOutput {
                    metadata: BTreeMap::new(),
                })
            }
        }
        H
    }

    fn input(path: &str) -> ValidateConfigInput {
        ValidateConfigInput {
            config_path: path.to_string(),
            core_type: "mock".into(),
            config_content: None,
            metadata: BTreeMap::new(),
        }
    }

    // -- Successful read populates config_content ----------------------------

    #[test]
    fn reads_file_into_config_content() {
        // Write a temp file with known content.
        let mut tmpfile = tempfile::NamedTempFile::new().expect("tmp file");
        let content = r#"{"outbounds": []}"#;
        tmpfile.write_all(content.as_bytes()).unwrap();
        let path = tmpfile.path().to_str().unwrap().to_string();

        // Intercept input inside pipeline using a spy hook.
        use std::sync::{Arc, Mutex};
        let captured: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let cap = captured.clone();

        let spy = FnHook::<OpValidateConfig>::new("spy").before(move |input| {
            *cap.lock().unwrap() = input.config_content.clone();
            Ok(())
        });

        let mut pipeline = Pipeline::new(Box::new(ok_handler()));
        pipeline.push_hook(Box::new(ConfigContentLoader::new()));
        pipeline.push_hook(Box::new(spy)); // runs after loader, sees populated content

        pipeline.execute(input(&path)).unwrap();

        let seen = captured.lock().unwrap().clone();
        assert_eq!(seen.as_deref(), Some(content));
    }

    // -- Missing file records error in metadata, does not abort ---------------

    #[test]
    fn missing_file_records_error_in_metadata_non_fatal() {
        use std::sync::{Arc, Mutex};
        let captured_meta: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let cap = captured_meta.clone();

        let spy = FnHook::<OpValidateConfig>::new("spy").before(move |input| {
            *cap.lock().unwrap() = input.metadata.get("config-content:error").cloned();
            Ok(())
        });

        let mut pipeline = Pipeline::new(Box::new(ok_handler()));
        pipeline.push_hook(Box::new(ConfigContentLoader::new()));
        pipeline.push_hook(Box::new(spy));

        // Path does not exist — should not abort pipeline.
        let result = pipeline.execute(input("/non/existent/path/config.json"));
        assert!(result.is_ok(), "pipeline must not abort on missing file");

        let meta = captured_meta.lock().unwrap().clone();
        assert!(meta.is_some(), "error must be recorded in metadata");
        assert!(meta.unwrap().contains("config.json"));
    }

    // -- Config content is None when file missing ----------------------------

    #[test]
    fn config_content_is_none_on_read_failure() {
        use std::sync::{Arc, Mutex};
        let captured: Arc<Mutex<Option<Option<String>>>> = Arc::new(Mutex::new(None));
        let cap = captured.clone();

        let spy = FnHook::<OpValidateConfig>::new("spy").before(move |input| {
            *cap.lock().unwrap() = Some(input.config_content.clone());
            Ok(())
        });

        let mut pipeline = Pipeline::new(Box::new(ok_handler()));
        pipeline.push_hook(Box::new(ConfigContentLoader::new()));
        pipeline.push_hook(Box::new(spy));

        pipeline.execute(input("/no/such/file.json")).unwrap();

        let seen = captured.lock().unwrap().clone().unwrap();
        assert!(
            seen.is_none(),
            "config_content must be None when file missing"
        );
    }

    // -- Pre-existing config_content is overwritten --------------------------

    #[test]
    fn overwrites_pre_existing_config_content() {
        let mut tmpfile = tempfile::NamedTempFile::new().expect("tmp file");
        tmpfile.write_all(b"real content").unwrap();
        let path = tmpfile.path().to_str().unwrap().to_string();

        use std::sync::{Arc, Mutex};
        let captured: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let cap = captured.clone();

        let spy = FnHook::<OpValidateConfig>::new("spy").before(move |input| {
            *cap.lock().unwrap() = input.config_content.clone();
            Ok(())
        });

        let mut pipeline = Pipeline::new(Box::new(ok_handler()));
        pipeline.push_hook(Box::new(ConfigContentLoader::new()));
        pipeline.push_hook(Box::new(spy));

        let mut inp = input(&path);
        inp.config_content = Some("stale content".into()); // pre-existing
        pipeline.execute(inp).unwrap();

        let seen = captured.lock().unwrap().clone();
        assert_eq!(seen.as_deref(), Some("real content")); // overwritten
    }
}
