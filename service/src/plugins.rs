//! Backward compatibility shim — re-exports from [`middleware`](crate::middleware).
//!
//! The plugin system has been replaced by typed middleware pipelines.
//! See [`crate::middleware`] for the new API.

pub use crate::middleware::*;
