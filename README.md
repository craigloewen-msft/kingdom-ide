# Kingdom IDE

**A command surface for coordinating many coding agents at once.**

Open your dev folder. Every project inside becomes a **city**. Agents working in
them are **architects**, who submit **plans** for your approval. You are the King.

The point is not editing code. The point is answering three questions at a glance:

1. What is every agent doing right now?
2. What shared resources are they holding, and who is blocked behind whom?
3. What are they proposing that I need to decide on?

## Running it

```bash
cargo leptos serve
```

Then open <http://127.0.0.1:3000> and give it the path to the folder that holds
your projects.

## Stack

Rust end to end — Axum on the server, Leptos (WASM) in the browser, with one
shared domain crate compiled into both. A `#[server]` function is a typed HTTP
call on the client and a direct call on the server from a single signature, so
there is no API client to hand-write and no schema to keep in sync.

```
crates/kingdom-core   domain model (no I/O; compiles to native + wasm)
crates/kingdom-app    Axum server + Leptos UI, split by feature flag
style/main.scss       styling
```

## Status

Early. Project scanning, the map, and the client/server round trip are real.
Architects, plans and resource leases are **placeholder data** — no agent is
actually spawned yet.

See [AGENTS.md](./AGENTS.md) for the architecture, the philosophy behind the
metaphor, and an honest breakdown of what is real versus faked.

## Tests

```bash
cargo test -p kingdom-core
```
