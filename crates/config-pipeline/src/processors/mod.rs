//! Built-in processors — the public surface.
//!
//! The public `core-config-processor` crate intentionally ships only
//! a **passthrough** processor and an **extism slot** for runtime
//! extensibility. Vendors that want to transform the config in
//! specific ways (DNS rewriting, ruleset URL → local-path localization,
//! stack tuning, routing exclusions, etc.) drop in a wasm plugin that
//! claims the `config.process` method — see [`extism`] for the adapter.
//!
//! ## Why the split
//!
//! The full native processor pipeline (with dns/ruleset/routing/stack
//! processors and their iteration logic) is proprietary operational
//! knowledge. It lives in a separate private crate that ships its
//! functionality as a wasm plugin the daemon loads at runtime.
//!
//! The public crate keeps the trait surface + a working default so
//! the OSS build is fully functional end-to-end: the daemon loads its
//! config, runs it through the (empty) pipeline, starts the core. If
//! a user wants the smart processors, they drop a wasm plugin into
//! the plugin dir and the extism slot picks it up.

pub mod extism;
pub mod passthrough;

pub use extism::ExtismProcessorAdapter;
pub use passthrough::PassthroughProcessor;
