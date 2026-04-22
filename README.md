# Pingling

Pingling is the public contract layer for extension hosts.

This repository intentionally contains only:

- stable slot names and payload envelopes;
- manifest and registry types for composing multiple extensions;
- deterministic ordering and conflict rules for slot ownership;
- host-function traits used by extension runners;
- primitive passthrough implementations for local tests and examples.

Product-specific core logic, config transformation, rule-set handling, platform
packaging, runtime policy, and service integration belong in downstream private
repositories. Public code must not encode those decisions.

## Crates

- `pingling-host-contract`: shared types and traits for slot-based extension
  calls, plugin manifests, and host-side registry planning.
- `pingling-primitive-host`: no-op passthrough implementation for consumers
  that need a host without product logic.

## Extension Shape

Extensions can declare the methods and slot phases they implement with a
`PluginManifest`. Hosts can index those manifests through `PluginRegistry` and
choose concrete execution behavior without learning product-specific details.

The public contract names the common daemon slots:

- `config.process`
- `deeplink.resolve`
- `auth.session`
- `vpn.connect`
- `vpn.disconnect`
- `ipc.dispatch`
- `plugin.load`

Slots run in phase order: `before`, `exec`, `after`. Multiple extensions can
bind the same slot when the slot policy permits it. Registry ordering is
deterministic: lower priority first, then plugin id as a stable tiebreaker.

Policies are explicit:

- `pipeline`: sequential transforms where each output becomes the next input.
- `first_success`: ordered handlers where the first concrete result wins.
- `single_owner`: exactly one extension may own that slot phase.
- `broadcast`: every extension observes; output is ignored by the host.
- `best_effort`: every extension runs and host policy may suppress failures.

## Boundary

The public boundary is the protocol. A downstream application owns how slots are
implemented and which extensions are loaded.
