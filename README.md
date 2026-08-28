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
built yet**.

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

## Documentation

[`AGENTS.md`](./AGENTS.md) is the working guide: the product, the naming rule,
the crate map, the invariants and the commands. [`docs/`](docs) holds the
references behind it — [architecture](docs/architecture.md), the
[agent loop](docs/agent-loop.md), [the map](docs/citymap.md) and
[shared resources](docs/shared-resources.md).
