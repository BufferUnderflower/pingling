# Plugin slots

**Status:** slot framework + 3 live slots implemented (2026-04-12).
**Supersedes** the dispatch details in `architecture-plugin.md`;
that doc is still authoritative for the public/private rationale.

## What is a "slot"?

A **slot** is a named extension point in the daemon where a wasm
plugin can inject behavior via three optional phases: `before`,
`exec`, `after`. The host walks the phases in order, folding each
phase's output payload into the next phase's input. Any phase the
plugin doesn't claim is skipped; any phase can short-circuit.

Slots are **aspect-oriented hooks** — one slot can be claimed by a
plugin that only cares about telemetry (`before` + `after` across
all slots), or by a plugin that completely replaces the daemon's
default behavior (`exec` returns `Halt`), or by a mix. The
framework is the same either way.

| Phase    | Typical use                                             |
|----------|---------------------------------------------------------|
| `before` | validation, auth, rate-limit, quota, span open          |
| `exec`   | the actual operation (transform, resolve, compute)      |
| `after`  | response mutation, telemetry emit, cleanup, span close  |

## How it works on the wire

**No new wasm ABI.** Plugins still export a single
`plugin_handle_ipc(method, params)` — the slot convention is
layered on top. For slot `my.slot` the host dispatches up to three
method names:

```
slot.my.slot.before
slot.my.slot.exec
slot.my.slot.after
```

with a [`SlotContext`](../domain/src/traits/plugin_slot.rs)
envelope:

```json
{
  "slot": "my.slot",
  "phase": "before",
  "wire_version": 1,
  "invocation_id": "17c91b-0",
  "payload": { ... slot-specific schema ... }
}
```

The plugin must return a tagged
[`SlotOutcome`](../domain/src/traits/plugin_slot.rs):

```json
{"kind": "unchanged"}
{"kind": "continue", "payload": { ... }}
{"kind": "halt", "payload": { ... }}
{"kind": "error", "message": "..."}
{"kind": "unhandled"}
```

| Variant      | Host reaction                                        |
|--------------|------------------------------------------------------|
| `unchanged`  | phase observed, chain continues with same payload    |
| `continue`   | phase returned new payload, chain folds it forward   |
| `halt`       | chain terminates, host uses payload as final result  |
| `error`      | chain terminates with error, host surfaces up        |
| `unhandled`  | plugin passes, host advances to the next phase       |

## Observer sink

Every phase transition is reported to an optional
[`SlotObserver`](../domain/src/traits/plugin_slot.rs). The daemon's
default observer (`ipc_server::BroadcastingSlotObserver`) has two
sinks:

1. **Log** on the `ipc_server::slot` target (default: trace for
   enter/unchanged/skipped, debug for continue/halt, warn for
   error). Operators enable with `RUST_LOG=ipc_server::slot=debug`.
2. **IPC broadcast** — every transition becomes an `event.slot.<kind>`
   notification on the same push channel as `event.stateChanged`.
   Subscribers see `event.slot.enter`, `event.slot.unchanged`,
   `event.slot.continue`, `event.slot.halt`, `event.slot.unhandled`,
   `event.slot.skipped`, and `event.slot.error`. Params:
   `{slot, phase, wire_version, invocation_id, payload, error?}`.

Subscribe with `event.subscribe` over JSON-RPC and filter client-side
on method name. **This is the default telemetry channel for daemon
v0.1.3 and later** — no plugin needed to get a full trace of every
slot invocation. Broadcasting can be turned off at boot with
`PINGLING_SLOT_BROADCAST=0` for hot-path deployments.

## Canonical slot catalog

### Live — slot + call site both implemented

| # | Slot                | Wired in            | Notes                                                |
|---|---------------------|---------------------|------------------------------------------------------|
| 1 | `vpn.connect`       | `service::VpnManager::connect`   | `before` pre-flight + `after` with connect duration |
| 2 | `vpn.disconnect`    | `service::VpnManager::disconnect`| Mirror of (1); `after` for session metrics          |
| 3 | `ipc.dispatch`      | `ipc_server::handle_line`        | Cross-cutting — fires on every JSON-RPC call        |

Each live slot has a typed payload struct in
[`domain::plugin_slot_payloads`](../domain/src/traits/plugin_slot_payloads.rs)
and a wire-version constant pinned at 1.

### Scaffolded — schema exists, call site pending

These slots have a payload struct + wire version, so a plugin can
be written against them today; the host doesn't yet dispatch to
them, so the chain is never fired. First caller to need one drops
in a one-liner that fires the chain and the slot becomes live.

| # | Slot                  | Pending call site                            | Why scaffolded now                                 |
|---|-----------------------|----------------------------------------------|----------------------------------------------------|
| 4 | `core.start`          | wrap `core.start(config_path)` in VpnManager | First real libbox integration will need this      |
| 5 | `core.stop`           | mirror of (4)                                | Final metrics snapshot, crash detection            |
| 6 | `profile.activate`    | `profile.activate` IPC method handler        | License check; audit trail                         |
| 7 | `profile.persist`     | `ProfileStorage::put` call site              | Secret redaction, policy enforcement              |
| 8 | `daemon.startup`      | CLI/headless main after plugins load         | Self-registration, warm-up, mTLS cert bootstrap    |
| 9 | `daemon.shutdown`     | SIGTERM/Ctrl-C handler                       | Persist in-flight state, flush telemetry           |
| 10| `outbound.select`    | `outbounds.select` IPC method handler        | Custom outbound picker, geo-routing                |
| 11| `outbound.test_latency` | `outbounds.testLatency` IPC method handler | Alternative latency probes, result normalization   |

**Future-use note:** `daemon.startup` is the natural spot for future
mTLS client cert rotation — the `after` phase sees a populated
`DaemonStartupPayload` and can call out to an enrollment server to
refresh a short-lived cert before the daemon starts accepting
connections. Design preserved; implementation when we ship mTLS.

### Future — listed here, no schema yet

These are documented to preserve design intent only. No payload
struct, no dispatch site. The table survives so future work has a
place to start; don't invent scenarios until a concrete caller
materializes.

| Slot                 | Shape sketch (not binding)                            | Example future use               |
|----------------------|--------------------------------------------------------|----------------------------------|
| `netwatch.event`     | `{kind, details}`                                     | Auto-reconnect on wifi change    |
| `log.emit`           | `{level, target, message, fields}`                    | Redirect logs to a sink, redact  |
| `update.check`       | `{current_version, channel}`                          | Subscription-tier update gating  |
| `config.validate`    | `{config, source}`                                    | Org-policy lint on top of sing-box native validation |
| `plugin.load`        | `{name, path, size_bytes}`                            | Meta-plugin orchestration        |

## Performance

Each slot dispatch costs up to 3× `handle_ipc` round-trips (one per
phase) plus one observer call per phase. Extism plugin calls are
~50–100µs each in-process; a fully-claimed slot on `ipc.dispatch`
adds ~200–400µs per client JSON-RPC call. If this shows up in p99,
the mitigation is a plugin-side "capability manifest" export that
pre-declares which phases the plugin claims, so the host can skip
the rest without the round-trip. Not needed in v0.1.3; noted here
to preserve the design hint.

## Versioning

Each payload struct pairs with a `*_WIRE_VERSION` const. Bump on
incompatible shape changes. Plugins that see an unexpected version
should return `SlotOutcome::Error` with a clear message ("unknown
wire version X for slot Y") rather than silently reinterpret
fields. Wire versions are independent per slot so one slot can
migrate to v2 without affecting others.

## Related reading

- [`architecture-plugin.md`](architecture-plugin.md) — public/private
  rationale, why the daemon has a plugin slot at all.
- [`domain/src/traits/plugin_slot.rs`](../domain/src/traits/plugin_slot.rs)
  — `SlotContext`, `SlotOutcome`, `run_slot_chain`, `run_slot_chain_observed`,
  `SlotObserver`, `NullSlotObserver`.
- [`domain/src/traits/plugin_slot_payloads.rs`](../domain/src/traits/plugin_slot_payloads.rs)
  — every live + scaffolded payload schema.
- [`ipc-server/src/slot_observer.rs`](../ipc-server/src/slot_observer.rs)
  — the broadcasting observer the daemon wires at startup.
- [`plugin-extism/tests/fixtures/plugin_mock/src/lib.rs`](../plugin-extism/tests/fixtures/plugin_mock/src/lib.rs)
  — smallest possible plugin that exercises every `SlotOutcome`
  variant. Good starting point for new plugin authors.
