# Kingdom IDE

**A command surface for coordinating many coding agents at once.**

Open your dev folder. Every project inside becomes a **city**. Work is proposed
as **architectural plans**, drafted by a model, which you approve or reject. You
are the King.

The point is not editing code. The point is answering three questions at a glance:

1. What is every agent doing right now?
2. What shared resources are they holding, and who is blocked behind whom?
3. What are they proposing that I need to decide on?

Question 2 is the goal, not the state of play: resource arbitration is **not
built yet**. See `AGENTS.md` §3 for what exists and what does not.

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
IDE. Set `KINGDOM_REALM=kingdom-mirror` as well and the server opens that realm
at startup rather than the folder picker, which is the rehearsal loop:

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
crates/kingdom-browser  headless Chrome over CDP (native only)
style/main.scss         styling
```

## How a holding is sized

Every file on the map is a building, and its dimensions are measured from the
file itself. The two axes carry two different facts.

**The base is how much code there is.** A folder's ground is divided among its
contents by weight — `20 + √bytes` for a file — so a big file gets a visibly
bigger plot than its neighbours: 64 KB buys about five times the lot area of
1 KB. What stays fixed is the *average* plot per file (`LAND_PER_FILE`), which
is why a 2,000-file repository grows a larger island rather than cramming the
same ground into smaller houses. One consequence worth knowing: a plot is a
*share of its folder*, so the same file looks bigger among small siblings than
among large ones.

**The height is how tangled it is** — branches per line of code, not length.
Length is already the base, and measured across this repository lines and total
branch count correlate at 0.94, so putting both on the map would draw one fact
twice; branch *density* is independent of size (0.06). A long plain file is
therefore a broad low hall, and a short knotty one stands up. The result is
clamped to 11–54 units, then capped at ~1.9× the footprint's shorter side so
nothing towers over the neighbours it would hide under the fixed camera angle.

**Everything else is category.** What a file is *for* fixes its archetype and
colour, identically in every repository — a test is a watchtower, docs are a
scriptorium, config is a council hall. Branch density also adds chimneys,
crates and forge stacks, so a complex file looks busy as well as tall.

### What "complexity" actually means

It is a **crude branch count**, not a claim about code quality. The scanner
counts occurrences of ten tokens — `if`, `for`, `while`, `match`, `switch`,
`catch`, `case`, `else`, `&&`, `||` — and divides by the lines of code. Its
limits are worth stating plainly:

- It is **substring matching, not parsing**. A `&&` inside a string literal
  counts. Tokens are space-padded, so `else` will not match inside an
  identifier, but that is the extent of the cleverness.
- **Comments and blank lines are excluded** from both the count and the
  divisor. They have to be: this crate's own `lib.rs` is 155 lines of which 98
  are prose, and phrases like "for exactly this reason" would otherwise score
  as branches and make the most heavily documented file look like the most
  tangled one.
- **Only code is scored.** Documentation, configuration, data and assets always
  score zero and sit at the minimum height by construction.
- Files over 2 MB and non-text files are never opened, so they also come out at
  the floor.

Treat a tall building as "this file has a lot of decisions per line, take a
look", not as a metric to optimise. The real rules live in
`crates/kingdom-citymap/src/build/layout.rs`.

## Status

Early. Project scanning, the map, and the client/server round trip are real.
So is drafting: a decree opens a plan and calls a model with that project's real
scan data.

Every model is chosen the same way, from one list in the decree bar. With a
working credential the picker opens on a real model; with none it offers the
offline **mock**, so a fresh clone drafts with no credential and no network —
not because the mock is a special mode, but because it is the only model left in
the list. To reach real models, copy `.kingdom.env.example` to `.kingdom.env`
and set either a token (`KINGDOM_API_KEY`) or a command that prints one
(`KINGDOM_API_KEY_HELPER`, defaulting to `agency auth github`). Note that this
means a configured clone spends tokens on its first decree; pick `mock` in the
picker to work offline.

Plans still cannot *do* anything beyond replying — no tool use, no edits — and
the court a kingdom opens with is placeholder data.

See [AGENTS.md](./AGENTS.md) for the architecture, the philosophy behind the
metaphor, and an honest breakdown of what is real versus faked.

## Tests

```bash
cargo test -p kingdom-core
cargo test -p kingdom-app --features ssr --no-default-features
```

No test launches a browser, so the suite needs nothing installed. Kingdom finds
Chrome itself at runtime — whatever is on `PATH` or in the usual install
locations, and failing that a Chromium that Playwright or Puppeteer already
downloaded. Set `KINGDOM_CHROME_EXECUTABLE` only to override that.
