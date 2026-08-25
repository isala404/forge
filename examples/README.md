# Canonical examples

Forge maintains one release-gated backend example per language. Rust uses [`todoapp/rust-be`](todoapp/rust-be), JavaScript uses [`chatapp/node-be`](chatapp/node-be), Python uses [`linksapp/python-be`](linksapp/python-be), and Go uses [`../bindings/go/examples/worker`](../bindings/go/examples/worker). CI builds and tests the backend boundary for each example.

The primitive guide carries the canonical OpenFeature provider setup for all four languages. Application examples keep direct flag calls where that is the smallest readable choice; production code can replace those calls with the official provider without changing stored rules. Startup configuration should use the new bulk reads, and disconnected work should use only explicitly expiring snapshots with a declared secret-handling mode.
