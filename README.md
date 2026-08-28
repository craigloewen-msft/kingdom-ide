<div align="center">

# Kingdom IDE

**A command surface for coordinating many coding agents at once.**

[![Rust](https://img.shields.io/badge/Rust-1.97-000?logo=rust)](rust-toolchain.toml)
[![Leptos](https://img.shields.io/badge/UI-Leptos%20%2F%20WASM-ef3939)](https://leptos.dev)
[![License](https://img.shields.io/badge/license-MIT-blue)](Cargo.toml)
![Status](https://img.shields.io/badge/status-early-orange)

</div>

<!-- Screenshot goes here: public/screenshot.png, once one is captured. -->

## What it is

Kingdom IDE is not an editor that happens to have AI in it. Open the folder that
holds your projects: each one becomes a **city**, and work is proposed as a
**plan** — drafted by a model in its own git worktree — that you approve or
reject. You are the King, and your scarce resource is judgement, not keystrokes.

It exists to answer three questions at a glance:

1. What is every agent doing right now?
2. What shared resources are they holding, and who is blocked behind whom?
3. What are they proposing that I need to decide on?

> Question 2 is the goal, not the state of play — resource arbitration is **not
> built yet**.

## Features

- **Many plans at once** — every plan gets its own git worktree, so agents never
  edit the same file underneath each other.
- **Isolation you choose per plan** — on this machine, with a network of its own,
  or on a machine of its own, so two agents can both bind `:3000`.
- **A map of your dev folder** — projects drawn as towns, rendered in Bevy.
- **Review and approve** — proposals arrive as annotated diffs, not as commits
  already made.
- **A browser the agent can drive** — headless Chrome over CDP, so a plan can
  check its own work.
- **Shared services** — Docker containers declared once and handed to every plan
  that reaches them.
- **The Proving Grounds** — a synthetic dev folder, generated on demand, for
  rehearsing against nothing real.

## Quick start

Prerequisites: Rust (the pinned toolchain installs itself from
`rust-toolchain.toml`) and `cargo-leptos`.

```bash
cargo install cargo-leptos
cargo leptos serve      # http://127.0.0.1:3000
```

Point it at the folder holding your projects — or press **“Enter the Proving
Grounds”** for a synthetic one. No credential is needed: with none configured,
Kingdom falls back to an offline mock model. Copy `.kingdom.env.example` to
`.kingdom.env` to change that.

When pointing Kingdom IDE at Kingdom IDE, rehearse against a realm instead:

```bash
KINGDOM_REALM=kingdom-mirror KINGDOM_SANDBOX=1 cargo leptos watch
```

- `KINGDOM_SANDBOX=1` — refuse any folder outside the proving grounds.
- `KINGDOM_REALM=<name>` — open that realm at startup instead of the picker.
- `cargo run -p kingdom-app --bin kingdom-seed -- --list` — seed realms by hand.

The fixtures are plain Rust in `crates/kingdom-core/src/mockdata/fixtures.rs`;
edit and re-seed with `--force`.

## Project layout

```
crates/kingdom-core      Domain model. No I/O — compiles to native and wasm32.
crates/kingdom-app       Axum server + Leptos UI, split by feature flag.
crates/kingdom-citymap   The map: projects as settlements, in Bevy.
crates/kingdom-browser   Headless Chrome over CDP (native only).
style/main.scss          All styling.
```

## Development

```bash
cargo test-all           # the whole native suite
cargo fmt --all
cargo clippy --workspace --all-targets --no-default-features --features kingdom-app/ssr
cargo check -p kingdom-app --target wasm32-unknown-unknown --features hydrate --no-default-features
```

## Documentation

[`AGENTS.md`](./AGENTS.md) is the working guide: the product, the naming rule,
the crate map and the invariants. [`docs/`](docs) holds the references —
[architecture](docs/architecture.md), the [agent loop](docs/agent-loop.md),
[the map](docs/citymap.md) and [shared resources](docs/shared-resources.md).

## License

MIT.
