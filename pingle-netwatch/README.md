# pingle-netwatch

Cross-platform network interface watcher for the pingle daemon.

## What it does

Wraps the [`netwatcher`](https://crates.io/crates/netwatcher) crate (which itself
abstracts `NotifyIpInterfaceChange` on Windows, `SystemConfiguration` on macOS,
and `netlink` on Linux) and exposes a `Watcher` trait + `UpdateEvent` channel
for the daemon to consume directly.

## Why a native crate, not a wasm plugin

Network change notifications are a *platform* abstraction, not a vendor concern.
The reactive event channel needs zero serialization overhead per interface
change, and wasm guests can't access platform syscalls without host-imported
functions — which means writing the platform code in Rust anyway. The wasm hook
slot here exists for *debugging the interpretation layer*, not for the platform
abstraction itself.

## Usage

```rust
use pingle_netwatch::{NetwatcherBackend, Watcher};

let watcher = NetwatcherBackend::new();
let rx = watcher.subscribe()?;
for event in rx.iter() {
    println!("{event:?}");
}
```

## Optional debug plugin slot

```rust
use pingle_netwatch::{NetwatchPlugin, PassthroughPlugin};

// Default — events flow through unchanged.
let plugin: Box<dyn NetwatchPlugin> = Box::new(PassthroughPlugin);
```

The plugin slot is described in the design spec at
`docs/superpowers/specs/2026-04-08-pingle-netwatch-config-pipeline-design.md`.
