# A proving ground that raises itself

Every rehearsal of a change starts with the same four unwritten steps: run
`cargo leptos serve`, open the browser, click **Enter the Proving Grounds**,
remember that `KINGDOM_SANDBOX=1` was the whole point and that nobody set it.
The server holds the open kingdom in a process-global `Mutex<Kingdom>`
(`api.rs::state`), initialised to `Kingdom::unopened()`, so **every restart**
sends you back to the folder picker — and `cargo leptos watch` restarts on every
save.

The machinery to avoid this already exists and is good: `mockdata` defines the
fixtures, `mock::seed` writes them, `enter_proving_grounds` seeds-if-absent and
opens. Nothing here needs building. What is missing is that **none of it is
reachable without a human clicking**, and no document tells an agent that
rehearsing against a proving ground is the expected way to work.

So this task is two small things: one environment variable that makes the
existing path automatic, and the written instruction that makes it the default
habit.

---

## 1. `KINGDOM_REALM` — open a proving ground at boot

```mermaid
flowchart LR
  Env["KINGDOM_REALM=kingdom-mirror"] --> Main["main.rs, after dotenvy"]
  Main --> Open["api::open_fixture(name)"]
  Open --> Seed["mock::seed — only if absent"]
  Seed --> Assemble["assemble() — the ordinary scanner"]
  Assemble --> State[("the process-global Kingdom")]
  Btn["⚔ Enter the Proving Grounds"] --> Open
```

Set it and the server comes up with that realm already open: the map is
populated, the rail has the opening court, `/plan/:id` is reachable on the first
request. Unset, nothing changes at all — the picker appears exactly as today.

**Where the work goes.** `enter_proving_grounds` in `api.rs` already does
precisely this job; it is just wrapped in a `#[server]` function that only a
browser can call. Extract its body into

```rust
#[cfg(feature = "ssr")]
pub fn open_fixture(name: &str) -> Result<Kingdom, String>
```

and have both the server function and the boot path call it. Same anti-drift
reasoning `assemble` is already documented with: the button and the environment
variable must not become two ways of opening a realm that differ in some detail
nobody notices until one of them is wrong. Note it is *not* `async` — every step
in it is blocking filesystem work today.

**Failure is a warning, not a panic.** An unknown realm name or a seed that
cannot be written prints to stderr and leaves the kingdom unopened, so the user
lands on the picker and can carry on. Refusing to boot over a convenience
setting would be a worse trade than the one being made.

The boot hook goes in `main.rs` immediately after the `dotenvy` call and before
the listener binds, so `.kingdom.env` can set it and the greeting line can say
what was opened:

```text
  ♚  Kingdom IDE — the throne room awaits at http://127.0.0.1:3000
     5 model(s) available, opening on mock — no credential found
     Opened the proving ground 'kingdom-mirror' at .kingdom/realms/kingdom-mirror
```

That last line matters more than it looks: the failure mode this whole feature
invites is doing real work against fake cities and not noticing. The banner is
where you find out.

**Why an environment variable rather than a CLI flag.** `cargo leptos serve`
owns the server's argv and passes nothing through, so a flag would be
unreachable from the one command everybody actually runs. The variable is also
the only form `.kingdom.env` can carry, which is what makes the setting stick
across a `watch` session instead of being retyped.

## 2. Say so, in the three places someone looks

**`AGENTS.md` §5** gets a short subsection — *Rehearsing a change* — directly
after the existing seed commands. One canonical block:

```bash
# Rehearsing a change: a populated map, on fake cities, every restart.
KINGDOM_REALM=kingdom-mirror KINGDOM_SANDBOX=1 cargo leptos watch
```

with the three sentences worth having beside it: `KINGDOM_REALM` seeds on first
use and is instant thereafter; `KINGDOM_SANDBOX=1` makes "I meant to open the
fake one" a rule the server enforces rather than something you remember; and if
you are changing what a fixture contains, re-seed with `--force` because an
already-standing realm is deliberately left alone. This is the section an agent
reads before touching anything, which is exactly why the instruction belongs
there rather than only in the README.

**`.kingdom.env.example`** gets `#KINGDOM_REALM=kingdom-mirror` in its existing
Proving Grounds block, beside `KINGDOM_SANDBOX` and `KINGDOM_SANDBOX_ROOT` —
that block is already the right home for it, and a commented line is how the
other two are offered.

**`README.md`** gets the same one-liner in *Running it*.

While there: the README points at
`crates/kingdom-core/src/mockdata/realms.rs`, which was renamed to `fixtures.rs`
by task 00100. Fix it. A wrong path in the one paragraph telling someone how to
change the fake data is worth the one-line diff.

---

## What this deliberately does not do

**No committed `.kingdom.dev.env` loaded automatically.** It sounds tidier and
is worse. Every value that would go in it — `KINGDOM_SANDBOX=1`,
`KINGDOM_MODEL=mock`, a realm name — is actively wrong for someone who cloned
Kingdom IDE to *use* it: a sandbox they did not ask for, silently refusing to
open their dev folder. The existing `.kingdom.env` (gitignored, copied from the
example) is already the mechanism for machine-local settings, and it works for
this with no new file and no new precedence rule to explain.

**No change to what a fixture contains, and no new fixture.** `kingdom-mirror`
is already documented as the modest, sub-second everyday realm. If rehearsal
later needs a shape none of the three provide, that is a `fixtures.rs` edit on
its own merits, not part of plumbing an environment variable.

**No auto-seeding inside `cargo test`.** The suite needs no proving ground on
disk: `mock.rs` and `store.rs` build their own scratch directories, and the
seed→scan roundtrip test seeds a fixture itself into a temp dir. Adding a global
test fixture would make a fast, hermetic suite depend on shared mutable state
under the repo.

## Tests

**None.** The behaviour this adds is a boot-time call to a function whose
substance is already pinned: `mock.rs` tests that a seeded fixture scans back as
declared and that an unmarked directory is refused, and `api.rs` tests that the
sandbox cannot be walked out of. A test asserting that reading an environment
variable calls the function it says it calls would restate the implementation,
and `std::env::set_var` in a test races every other test in the binary — which
`within_sandbox` was already split out specifically to avoid.

The honest verification is the change itself: with `KINGDOM_REALM` set, the
server's own banner names the realm it opened, and the map is populated on the
first request.

## Definition of done

- `KINGDOM_REALM=kingdom-mirror cargo leptos serve` comes up on a populated map,
  with no click, and prints which realm it opened.
- A second run is instant, and does not re-seed a standing realm.
- `KINGDOM_REALM=nonesuch` prints a warning naming the known realms and lands on
  the picker; the server serves normally.
- Unset, behaviour is byte-for-byte what it is today.
- The button and the boot path go through one shared function.
- `AGENTS.md`, `README.md` and `.kingdom.env.example` all give the same command,
  and the README's `realms.rs` path is corrected to `fixtures.rs`.
- `cargo test -p kingdom-core` and
  `cargo test -p kingdom-app --features ssr --no-default-features` pass.
