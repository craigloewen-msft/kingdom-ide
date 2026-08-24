# Kingdom IDE

**A command surface for coordinating many coding agents at once.**

Open your dev folder. Every project inside becomes a **city**. Work is proposed
as **architectural plans**, drafted by a model, which you approve or reject. You
are the King.

The point is not editing code. The point is answering three questions at a glance:

1. What is every agent doing right now?
2. What shared resources are they holding, and who is blocked behind whom?
3. What are they proposing that I need to decide on?

## Running it

```bash
cargo leptos serve
```

Then open <http://127.0.0.1:3000>.

You can point it at the folder that holds your projects — or, better while
exploring, press **"Enter the Proving Grounds"** for a synthetic dev folder
generated on demand. Nothing real is touched, and the same realm comes out the
same way every time.

```bash
# Or from the CLI:
cargo run -p kingdom-app --bin kingdom-seed -- --list
cargo run -p kingdom-app --bin kingdom-seed -- kingdom-mirror
```

Set `KINGDOM_SANDBOX=1` and the server will refuse to open anything outside the
proving grounds — worth doing whenever you use Kingdom IDE to work on Kingdom
IDE. The fake data is plain Rust in `crates/kingdom-core/src/mockdata/realms.rs`;
edit it and re-seed with `--force`.

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
So is drafting: a decree opens a plan, takes a lease on the city's files, and
calls a model with that project's real scan data.

Out of the box an offline **mock** model drafts every plan, so a fresh clone
works with no credential and no network. To use a real model, copy
`.kingdom.env.example` to `.kingdom.env` and set `KINGDOM_MODEL_PROVIDER=copilot`
plus either a token (`KINGDOM_API_KEY`) or a command that prints one
(`KINGDOM_API_KEY_HELPER`, defaulting to `agency auth github`).

Plans still cannot *do* anything beyond replying — no tool use, no edits — and
the court a kingdom opens with is placeholder data.

See [AGENTS.md](./AGENTS.md) for the architecture, the philosophy behind the
metaphor, and an honest breakdown of what is real versus faked.

## Tests

```bash
cargo test -p kingdom-core
cargo test -p kingdom-app --features ssr --no-default-features
```
