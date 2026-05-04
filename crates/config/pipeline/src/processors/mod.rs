//! Built-in processors — the public surface.
//!
//! The public `core-config-processor` crate intentionally ships only
//! a **passthrough** processor. Runtime extensibility belongs to
//! WIT-described component packages wired by the downstream host
//! composition root.
//!
//! ## Why the split
//!
//! The full native processor pipeline (with dns/ruleset/routing/stack
//! processors and their iteration logic) is proprietary operational
//! knowledge. It lives in a separate private crate that ships its
//! functionality as component packages the daemon loads at runtime.
//!
//! The public crate keeps the trait surface + a working default so
//! the OSS build is fully functional end-to-end: the daemon loads its
//! config, runs it through the (empty) pipeline, starts the core. If
//! a user wants the smart processors, they drop a wasm plugin into
//! the component package dir and the host wires it into this pipeline.

pub mod passthrough;

pub use passthrough::PassthroughProcessor;
