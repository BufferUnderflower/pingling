//! Geo-filter hook — filters outbounds by country whitelist.
//!
//! [`GeoFilterHook`] implements `Hook<OpListOutbounds>` using only the `after`
//! phase: after the handler returns the full outbound list, this hook removes
//! any outbound whose `country_code` is not in the allowlist.
//!
//! Outbounds without a `country_code` are **kept** (fail-open policy: unknown
//! location is better than silently dropping a valid server).
//!
//! # Usage
//!
//! ```rust,ignore
//! use service::middleware::geo_filter::GeoFilterHook;
//!
//! list_outbounds_pipeline.push_hook(Box::new(
//!     GeoFilterHook::new(vec!["JP".into(), "DE".into(), "US".into()])
//! ));
//! ```
//!
//! # Why `after` instead of `before`
//!
//! The handler produces the list — filtering is an output transformation, not
//! an input gate. Using `after` is the natural fit: it receives the full list
//! and returns a filtered subset.
//!
//! # Extism equivalent
//!
//! A WASM plugin achieves the same by exporting `filter_outbounds`, which
//! receives the list and returns the IDs to keep. See `plugin-extism` for
//! the adapter that wires this into `Hook<OpListOutbounds>::after`.

use domain::ops::{ListOutboundsInput, ListOutboundsOutput, OpListOutbounds};
use domain::pipeline::Hook;
use domain::VpnError;

/// Filters outbounds to the allowed ISO 3166-1 alpha-2 country codes.
///
/// Outbounds without a `country_code` are kept (fail-open).
pub struct GeoFilterHook {
    allowed: Vec<String>,
}

impl GeoFilterHook {
    pub fn new(allowed_countries: Vec<String>) -> Self {
        Self {
            allowed: allowed_countries,
        }
    }
}

impl Hook<OpListOutbounds> for GeoFilterHook {
    fn name(&self) -> &str {
        "example:geo-filter"
    }

    /// Filters the outbound list by country whitelist.
    ///
    /// Called after the handler has produced the full list. Removes any
    /// outbound whose `country_code` is set and not in `allowed`. Records
    /// a `geo-filter:removed` count in output metadata.
    fn after(
        &self,
        _input: &ListOutboundsInput,
        output: &mut ListOutboundsOutput,
    ) -> Result<(), VpnError> {
        let before = output.outbounds.len();
        output.outbounds.retain(|o| {
            o.country_code
                .as_ref()
                .map(|cc| self.allowed.contains(cc))
                .unwrap_or(true) // keep if country unknown
        });
        let removed = before - output.outbounds.len();
        if removed > 0 {
            output
                .metadata
                .insert("geo-filter:removed".into(), removed.to_string());
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
    use domain::pipeline::{Handler, Pipeline};
    use domain::types::{Outbound, OutboundProtocol, OutboundTransport};

    fn make_outbound(id: &str, cc: Option<&str>) -> Outbound {
        Outbound {
            id: id.into(),
            name: id.into(),
            protocol: OutboundProtocol::Vless,
            transport: OutboundTransport::Tcp,
            country_code: cc.map(|s| s.into()),
            location: None,
            latency_ms: None,
            selected: false,
            metadata: Default::default(),
        }
    }

    struct StaticHandler(Vec<Outbound>);
    impl Handler<OpListOutbounds> for StaticHandler {
        fn handle(&self, _: ListOutboundsInput) -> Result<ListOutboundsOutput, VpnError> {
            Ok(ListOutboundsOutput {
                outbounds: self.0.clone(),
                metadata: Default::default(),
            })
        }
    }

    fn input() -> ListOutboundsInput {
        ListOutboundsInput {
            core_type: "mock".into(),
            config_path: None,
            metadata: Default::default(),
        }
    }

    #[test]
    fn filters_non_whitelisted_countries() {
        let handler = StaticHandler(vec![
            make_outbound("jp-1", Some("JP")),
            make_outbound("ru-1", Some("RU")),
            make_outbound("de-1", Some("DE")),
            make_outbound("us-1", Some("US")),
        ]);

        let mut pipeline = Pipeline::new(Box::new(handler));
        pipeline.push_hook(Box::new(GeoFilterHook::new(vec!["JP".into(), "DE".into()])));

        let output = pipeline.execute(input()).unwrap();
        let ids: Vec<&str> = output.outbounds.iter().map(|o| o.id.as_str()).collect();
        assert_eq!(ids, vec!["jp-1", "de-1"]);
        assert_eq!(
            output
                .metadata
                .get("geo-filter:removed")
                .map(|s| s.as_str()),
            Some("2")
        );
    }

    #[test]
    fn keeps_outbounds_without_country_code() {
        let handler = StaticHandler(vec![
            make_outbound("jp-1", Some("JP")),
            make_outbound("unknown-1", None), // no country — must be kept
        ]);

        let mut pipeline = Pipeline::new(Box::new(handler));
        pipeline.push_hook(Box::new(GeoFilterHook::new(vec!["JP".into()])));

        let output = pipeline.execute(input()).unwrap();
        assert_eq!(output.outbounds.len(), 2); // both kept
        assert!(output.metadata.get("geo-filter:removed").is_none());
    }

    #[test]
    fn all_allowed_no_metadata_entry() {
        let handler = StaticHandler(vec![
            make_outbound("jp-1", Some("JP")),
            make_outbound("de-1", Some("DE")),
        ]);

        let mut pipeline = Pipeline::new(Box::new(handler));
        pipeline.push_hook(Box::new(GeoFilterHook::new(vec!["JP".into(), "DE".into()])));

        let output = pipeline.execute(input()).unwrap();
        assert_eq!(output.outbounds.len(), 2);
        // No removals → no metadata entry written
        assert!(output.metadata.get("geo-filter:removed").is_none());
    }

    #[test]
    fn empty_allowlist_removes_all_with_country() {
        let handler = StaticHandler(vec![
            make_outbound("jp-1", Some("JP")),
            make_outbound("unknown-1", None), // kept (no country)
        ]);

        let mut pipeline = Pipeline::new(Box::new(handler));
        pipeline.push_hook(Box::new(GeoFilterHook::new(vec![]))); // nothing allowed

        let output = pipeline.execute(input()).unwrap();
        // Only the unknown-country outbound survives.
        assert_eq!(output.outbounds.len(), 1);
        assert_eq!(output.outbounds[0].id, "unknown-1");
    }

    #[test]
    fn geo_filter_does_not_reject_on_success() {
        // after() must return Ok even when outbounds are removed.
        let handler = StaticHandler(vec![make_outbound("us-1", Some("US"))]);
        let mut pipeline = Pipeline::new(Box::new(handler));
        pipeline.push_hook(Box::new(GeoFilterHook::new(vec!["JP".into()])));

        // All outbounds removed but the operation itself succeeds (returns Ok).
        let result = pipeline.execute(input());
        assert!(result.is_ok());
        assert_eq!(result.unwrap().outbounds.len(), 0);
    }

    #[test]
    fn geo_filter_is_noop_when_list_empty() {
        let handler = StaticHandler(vec![]);
        let mut pipeline = Pipeline::new(Box::new(handler));
        pipeline.push_hook(Box::new(GeoFilterHook::new(vec!["JP".into()])));

        let output = pipeline.execute(input()).unwrap();
        assert!(output.outbounds.is_empty());
        assert!(output.metadata.is_empty());
    }

    #[test]
    fn on_error_not_fired_on_success() {
        use domain::pipeline::FnHook;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let on_error_ran = Arc::new(AtomicBool::new(false));
        let flag = on_error_ran.clone();

        let handler = StaticHandler(vec![make_outbound("jp-1", Some("JP"))]);
        let mut pipeline = Pipeline::new(Box::new(handler));
        pipeline.push_hook(Box::new(GeoFilterHook::new(vec!["JP".into()])));
        pipeline.push_hook(Box::new(
            FnHook::<OpListOutbounds>::new("spy")
                .on_error(move |_, _| flag.store(true, Ordering::SeqCst)),
        ));

        pipeline.execute(input()).unwrap();
        assert!(!on_error_ran.load(Ordering::SeqCst));
    }
}
