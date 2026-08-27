# Kingdom IDE

**A command surface for coordinating many coding agents at once.**

Open your dev folder. Every project inside becomes a **city**. Work is proposed
as **architectural plans**, drafted by a model, which you approve or reject. You
are the King.

The point is not editing code. The point is answering three questions at a
glance:

1. What is every agent doing right now?
2. What shared resources are they holding, and who is blocked behind whom?
3. What are they proposing that I need to decide on?

Question 2 is the goal, not the state of play: resource arbitration is **not
built yet**. [`docs/roadmap.md`](docs/roadmap.md) is the honest ledger.

## Running it

```bash
cargo leptos serve
```

Then open <http://127.0.0.1:3000>.

Point it at the folder that holds your projects — or, better while exploring,
press **"Enter the Proving Grounds"** for a synthetic dev folder generated on
demand. Nothing real is touched, and the same realm comes out the same way every
time.

```bash
# Or from the CLI:
cargo run -p kingdom-app --bin kingdom-seed -- --list
cargo run -p kingdom-app --bin kingdom-seed -- kingdom-mirror
```

`KINGDOM_SANDBOX=1` makes the server refuse anything outside the proving
grounds; `KINGDOM_REALM=kingdom-mirror` opens that realm at startup instead of
the folder picker. Together they are the rehearsal loop, and worth using
whenever you point Kingdom IDE at Kingdom IDE:

```bash
KINGDOM_REALM=kingdom-mirror KINGDOM_SANDBOX=1 cargo leptos watch
```

The fake data is plain Rust in `crates/kingdom-core/src/mockdata/fixtures.rs`;
edit it and re-seed with `--force`.

## Stack

Rust end to end — Axum on the server, Leptos (WASM) in the browser, with one
shared domain crate compiled into both. A `#[server]` function is a typed HTTP
call on the client and a direct call on the server from a single signature, so
there is no API client to hand-write and no schema to keep in sync.

```
crates/kingdom-core     domain model (no I/O; compiles to native + wasm)
crates/kingdom-app      Axum server + Leptos UI, split by feature flag
crates/kingdom-citymap  the map: projects drawn as towns, in Bevy
crates/kingdom-browser  headless Chrome over CDP (native only)
style/main.scss         styling
```

## Status

Early, and honest about it. Project scanning, the map and the client/server
round trip are real. So are plans: a plan works in its own git worktree with
hands — reading and searching files, patching them, `bash` and `tmux`, a
headless browser, profiling and subagents. It puts a proposal to you, you
approve or annotate it, and the work is merged or archived.

Every model is chosen from one list in the decree bar. With a credential the
picker opens on a real model; with none it offers the offline **mock**, so a
fresh clone drafts with no network. To reach real models, copy
`.kingdom.env.example` to `.kingdom.env` and set either `KINGDOM_API_KEY` or a
command that prints one (`KINGDOM_API_KEY_HELPER`). A configured clone spends
tokens on its first decree — pick `mock` to work offline.

Still placeholder: the court a kingdom opens with is fabricated data, and
resource arbitration — question 2 above — is **not built**.

## Tests

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --features ssr --no-default-features
cargo test -p kingdom-core
cargo test -p kingdom-app --features ssr --no-default-features
cargo test -p kingdom-citymap
cargo test -p kingdom-browser

# The client half is a separate target and a separate compile, so it is only
# ever proven by building it:
cargo check -p kingdom-app --target wasm32-unknown-unknown \
    --features hydrate --no-default-features
```

No test launches a browser, so the suite needs nothing installed. Kingdom finds
Chrome itself at runtime — `PATH`, the usual install locations, or a Chromium
Playwright or Puppeteer already downloaded. Set `KINGDOM_CHROME_EXECUTABLE` only
to override a wrong guess.

## Documentation

[`AGENTS.md`](./AGENTS.md) is the working guide: the product, the naming rule,
the crate map, the invariants and the commands. [`docs/`](docs) holds the
long-form references — [architecture](docs/architecture.md), the
[agent loop](docs/agent-loop.md),
[review and editing](docs/review-and-editing.md), [the map](docs/citymap.md) and
the [roadmap](docs/roadmap.md). [`tasks/`](tasks) holds one file per past change.
