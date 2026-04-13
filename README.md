# flux-memory

> In-memory key-value store with TTL, versioning, snapshots, and diff for the FLUX agent fleet.

## What This Is

`flux-memory` provides the **shared memory substrate** for FLUX agents — a Rust crate offering a key-value `Store` with time-to-live expiration, versioned entries, read-only protection, prefix search, snapshots, restore, and diff capabilities.

## Role in the FLUX Ecosystem

Every agent in the FLUX fleet needs persistent-but-evanescent state. `flux-memory` is the foundational memory layer that other fleet components build upon:

- **`flux-profiler`** stores profiling snapshots in memory during analysis
- **`flux-social`** caches agent relationship graphs
- **`flux-navigate`** persists waypoint and grid state
- **`flux-trust`** holds trust scores alongside observation counts

## Key Features

| Feature | Description |
|---------|-------------|
| **TTL Expiration** | Entries auto-expire after configurable seconds; `gc()` purges dead entries |
| **Versioned Entries** | Every `put` increments the version; `update` tracks modifications |
| **Read-Only Entries** | Immutable keys that reject `update()` calls |
| **Prefix Search** | `search("user:")` returns all matching `MemEntry` refs |
| **Snapshots & Restore** | `snapshot("label")` captures state; `restore()` rolls back |
| **Diff** | Compare current store against a snapshot to find added/removed keys |

## Quick Start

```rust
use flux_memory::Store;

let mut store = Store::new();

// Write with TTL (0 = never expires)
store.put("agent:name", "Super Z", 0, false);
store.put("session:token", "abc123", 3600, true); // 1-hour TTL, read-only

// Read
assert_eq!(store.get("agent:name"), Some("Super Z".to_string()));

// Search by prefix
let results = store.search("agent:");
assert_eq!(results.len(), 1);

// Snapshot and diff
let snap = store.snapshot("pre-flight");
store.put("agent:name", "Updated", 0, false);
let (added, removed) = store.diff(&snap);
```

## Building & Testing

```bash
cargo build
cargo test
```

## Related Fleet Repos

- [`flux-runtime`](https://github.com/SuperInstance/flux-runtime) — Python runtime with full VM, uses similar memory patterns
- [`flux-profiler`](https://github.com/SuperInstance/flux-profiler) — Profiling tool that benefits from memory snapshots
- [`flux-trust`](https://github.com/SuperInstance/flux-trust) — Trust scoring with Bayesian updates
- [`flux-evolve`](https://github.com/SuperInstance/flux-evolve) — Behavioral evolution engine
- [`flux-core`](https://github.com/SuperInstance/flux-core) — Core FLUX VM in Rust

## License

Part of the [SuperInstance](https://github.com/SuperInstance) FLUX fleet.
