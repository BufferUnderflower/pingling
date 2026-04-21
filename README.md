# Pingling

Pingling is the public contract layer for extension hosts.

This repository intentionally contains only:

- stable slot names and payload envelopes;
- host-function traits used by extension runners;
- primitive passthrough implementations for local tests and examples.

Product-specific core logic, config transformation, rule-set handling, platform
packaging, runtime policy, and service integration belong in downstream private
repositories. Public code must not encode those decisions.

## Crates

- `pingling-host-contract`: shared types and traits for slot-based extension
  calls.
- `pingling-primitive-host`: no-op passthrough implementation for consumers
  that need a host without product logic.

## Boundary

The public boundary is the protocol. A downstream application owns how slots are
implemented and which extensions are loaded.
