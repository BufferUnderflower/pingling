# pingle-config-pipeline

Native config processor pipeline + strategy iteration types for the
pingle daemon. Direct port of the dart `singbox_config` package.

## What it does

- **Native processors** (pure JSON-in / JSON-out transforms): `dns`,
  `ruleset`, `routing_excl`, `stack`, `log`, `clash_api`, `platform`.
- **Bulletproof native ruleset downloader** with on-disk cache. Sing-box's
  own ruleset fetcher is flaky on 20–50% of users on Windows; this
  processor downloads + caches every remote ruleset and rewrites the
  config so sing-box only ever sees `local` rule_set entries.
- **Strategy iteration types**: `ConnectionStrategy`, `RetryPolicy`,
  `StrategyPlan`. Direct port of the dart `ConnectionStrategy` +
  `RetryPolicy`.
- **Per-attempt envelope**: `AttemptInfo`, `ConfigRequest` — threaded
  through both the native pipeline and the plugin protocol so plugin
  authors see exactly what the native side saw.
- **Error taxonomy**: `ErrorKind`, `PreviousError`, `classify_error()`.
  Walks `VpnError` variants + message text to bin into a small stable
  taxonomy the strategy retry loop branches on.

## What this crate does NOT own

- The plugin slot. That's `pingle-pipeline-plugin`.
- The retry orchestrator. That's `service::middleware::strategy_retry`.
- The wire-format protocol shape. That's documented in the design spec.

## Error → action table (used by the strategy retry loop)

| `ErrorKind` | Action |
|-------------|--------|
| `DnsFailure` | Retry until exhausted, then advance strategy |
| `TcpTimeout` | Retry until exhausted, then advance strategy |
| `TcpRefused` | Retry until exhausted, then advance strategy |
| `TlsHandshake` | Retry until exhausted, then advance strategy |
| `HttpError` | Retry until exhausted, then advance strategy |
| `Timeout` | Retry until exhausted, then advance strategy |
| `Unknown` | Retry until exhausted, then advance strategy (best effort) |
| `Validation` | Advance strategy immediately (current strategy produced invalid config) |
| `AuthFailure` | Bail (api plugin owns refresh) |
| `TunDevice` | Bail (host-level, not strategy-fixable) |
| `PermissionDenied` | Bail |
| `PrerequisiteMissing` | Bail (e.g. libbox.dll missing) |

The action mapping lives in `service::middleware::strategy_retry`; this
crate just owns the classification.

## Design

See `docs/superpowers/specs/2026-04-08-pingle-netwatch-config-pipeline-design.md`.
