# AGENTS.md

Guidance for any agent (or human) working on **Kingdom IDE**. Read this before
writing code here. The long-form references live in [`docs/`](docs); the
blow-by-blow of each past change lives in [`tasks/`](tasks).

> Keep this file well under **64 KB**. `MOST_GUIDANCE` in
> `crates/kingdom-app/src/llm/system_prompt.rs` discards a guidance file that
> does not fit, so an over-long AGENTS.md reaches no model at all.

## 1. What this product is

Kingdom IDE is **not** an editor that happens to have AI in it. It is a
**command surface for coordinating many agents at once**, and it exists because
of a concrete failure: when several agents work across several projects on one
machine, they collide. Two bind port 3000. Two run `cargo build` against the
same target directory. One rewrites a file another is halfway through reading.
The work is individually fine and collectively broken.

It answers three questions, in priority order:

1. **What is every agent doing right now?**
2. **What shared resources are they holding, and who is blocked behind whom?**
3. **What are they proposing that I need to decide on?**

If a change does not serve one of those, it is probably not the most valuable
thing to build next. The guiding test:

> Does this make it easier for one person to know and steer what many agents
> are doing?

A beautiful file tree fails that test. A red line on the map between two
projects fighting over a port passes it.

The user is the King: a sovereign reviewing proposals, not a typing-assisted
programmer. His scarce resource is *attention and judgement*, not keystrokes.

## 2. Naming: metaphor in the UI, standard names in the code

**The metaphor is presentation, not domain.** It belongs in everything the King
reads, and nowhere the compiler reads.

| Layer | Vocabulary |
|---|---|
| UI copy — every string the King sees | **metaphor**: "The court sent an errand" |
| CSS class names, `style/` | **metaphor**: `.deed-mark`, `.chamber-log` |
| Type names, functions, variables, modules | **standard**: `ToolCall`, `Permissions` |
| Doc comments and commit messages | **standard**: "the model", "a tool call" |

The reason is asymmetric cost. In the UI the metaphor is the product's voice and
costs a reader nothing. In the code it is a second vocabulary every reader must
learn before they can read a match arm — and one that no error message, crate
doc or Stack Overflow answer will ever use back.

The translation, in full:

| Shown as | Called in code |
|---|---|
| a deed | `ToolCall`, `ToolOutcome`, `Entry::Tool` |
| the Court | `Speaker::Assistant` |
| the King | `Speaker::User`, "the user" |
| an errand | a subagent — `Plan::spawned`, `SpawnedBy`, `subagents_of` |
| a decree | a prompt — `slug_for_prompt`, `PromptBar` |
| the chamber | the conversation — `Conversation`, `conversation.rs` |
| the spyglass | the screencast — `screencast.rs`, `BrowserView` |
| the herald | the event bus — `events::publish`, `events::subscribe` |
| a remit | `Permissions::ReadOnly` / `Permissions::Full` |
| on this machine / a network of its own / a machine of its own | `Isolation::Shared` / `Isolated` / `Sealed` |
| a workshop | `Sandbox` |
| a well | a shared container — `ServiceSpec`, `RunningService`, `SharedService` |
| a folder let in | `MountSpec`, `MountMode`, `namespaces::mount` |
| shared resources (the screen) | `SharedResource`, `ResourceInventory`, `wells.rs` |
| a realm (fixture) | `FixtureSpec`, `fixtures.rs` |
| a ward | `Language` |
| a district / building | `Folder` / `SourceFile` |

`Kingdom`, `City` and `Plan` are deliberately **both** — crate names, routes and
the `.kingdom/` directory, and also ordinary English for a folder, a project and
a unit of proposed work. They need no translation.

The one exception is `kingdom-citymap`, which keeps `Ward`, `Town`, `Road`,
`Plaza` and `Holding`: that code draws a literal map of projects as settlements,
so there the metaphor *is* the subject matter. **Beware the collision** — in
that crate a `Ward` is a **folder**; everywhere else "a ward" is `Language`.
Nothing outside `kingdom-citymap` names a `Ward`, and its `lib.rs` says so.

When you add a concept, name the type for what it is and let the view call it
what the King calls it. If you find yourself writing a glossary comment to
explain an identifier, that is the signal you named it for the UI.

## 3. Architecture

Rust end to end. Axum server, Leptos (WASM) browser UI, one shared domain crate.

```
crates/kingdom-core     Domain model. No I/O, no framework deps.
                        Compiles to BOTH native and wasm32.
crates/kingdom-app      Axum server + Leptos UI in one crate, split by
                        feature flag. Server functions, the agent loop,
                        tools, LLM providers, components.
crates/kingdom-citymap  The map: projects drawn as towns, in Bevy.
crates/kingdom-browser  Headless Chrome over CDP (native only).
style/main.scss         All styling.
```

Module-by-module detail is in [`docs/architecture.md`](docs/architecture.md).

**Why one crate builds two targets.** `kingdom-app` compiles twice: natively
with `--features ssr` into the Axum server, and to `wasm32` with
`--features hydrate` into the browser bundle. A `#[server]` function is a real
HTTP call on the client and a direct call on the server, from **one** signature
— so there is no hand-written API client and no schema to keep in sync. Change a
field on `City` and both sides fail to compile together, rather than one side
failing at runtime in front of a user.

**Consequence to respect:** anything in `kingdom-core` must compile to wasm. No
`tokio`, no `std::fs`, no native-only crates. Server-only code goes in
`kingdom-app` behind `#[cfg(feature = "ssr")]`.

**Where state lives.** Everything Kingdom records about itself is in the King's
own profile — `~/.kingdom/`, or wherever `KINGDOM_HOME` points — not inside the
folder he opened:

```
~/.kingdom/
  settings.json                durable IDE settings; today, the last kingdom opened
  services.toml                shared resources the King keeps for every project
  kingdoms/<key>/
    kingdom.json               which root this folder is for
    plans/<plan-id>.json       one document per plan
    plans/<id>--<slug>.md      the plan itself, filed when its worktree goes
    archive/<plan-id>.patch    the work an archived plan set aside
  realms/<name>/               the proving grounds
```

Cities are **not** stored — they are rescanned every open, because disk is their
source of truth. Plans are, because a plan owns a worktree and forgetting it
orphans real work. `store.rs` is the seam. The full reasoning, including the
`<city>/.kingdom/` division, is in
[`docs/architecture.md`](docs/architecture.md#where-state-lives).

## 4. What is real and what is not

**Real:** project scanning, the map, the client/server round trip, and plans
that act — a plan works in its own git worktree with tools for reading,
searching, patching, `bash`, `tmux`, a headless browser, profiling and
subagents. It drafts to `.kingdom/draft.md`, calls `propose_plan`, and waits;
the King approves or annotates, and the work is merged or archived. A plan can
also be given a **network of its own** — its own `:3000`, forwarded back to a
host port the chamber shows — with a terminal into it. Off by default, chosen
per plan; see [`docs/architecture.md`](docs/architecture.md#a-network-of-a-plans-own).
A project can also declare **shared services** it needs standing — a database,
say — which Kingdom starts once for the whole city and every plan reaches at one
address; the King can also keep his own, shared by every project he opens. There
is a screen for seeing and declaring both:
[`docs/shared-resources.md`](docs/shared-resources.md).

**Faked:** the *opening* court — the plans a kingdom starts with, before any
decree (`kingdom_core::sample::starter_plans`). Plans the King opens are real.
The sample deliberately includes a failed plan, one mid-draft and one with a
standing proposal; do not "clean up" those states away. A test pins it.

**Not built:** resource arbitration beyond ports — question 2 above is only
half-answered, because a network namespace *avoids* a port clash rather than
detecting or reporting one, and a shared `target/` still blocks. Also:
subagents with tools, subagents while proposing, restoring an archived plan, and
live updates on the map. See [`docs/roadmap.md`](docs/roadmap.md) for why each
gap is its own decision.

How the loop actually works, and every failure it has been taught to survive, is
in [`docs/agent-loop.md`](docs/agent-loop.md). The review and editing surfaces
are in [`docs/review-and-editing.md`](docs/review-and-editing.md).

## 5. Running it

```bash
cargo leptos serve      # build + serve at http://127.0.0.1:3000
cargo leptos watch      # same, with rebuild on change
```

**Rehearse against a proving ground, not your dev folder:**

```bash
KINGDOM_REALM=kingdom-mirror KINGDOM_SANDBOX=1 cargo leptos watch
```

`KINGDOM_REALM` opens that realm at boot, so the server comes up on a populated
map rather than whichever kingdom was last opened — which matters under `watch`,
where every save restarts the server. It seeds on first use and is instant
thereafter. `KINGDOM_SANDBOX=1` makes "I meant to open the fake one" a rule the
server enforces: any folder outside the sandbox root is refused. Both belong in
`.kingdom.env` (gitignored; copy `.kingdom.env.example`).

```bash
cargo run -p kingdom-app --bin kingdom-seed -- --list
cargo run -p kingdom-app --bin kingdom-seed -- kingdom-mirror [--force]
```

To change the fake data, edit `crates/kingdom-core/src/mockdata/fixtures.rs` and
re-seed with `--force` — an already-standing realm is deliberately left alone.
It is plain Rust, so a mistyped fixture fails to compile rather than at seed
time.

### The full check before you hand work back

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --features ssr --no-default-features
cargo test -p kingdom-core
cargo test -p kingdom-app --features ssr --no-default-features
cargo test -p kingdom-citymap
cargo test -p kingdom-browser

# `kingdom-app` compiles TWICE and the suites above only build the native half.
# The wasm half is proven by building it, and nothing else does:
cargo check -p kingdom-app --target wasm32-unknown-unknown \
    --features hydrate --no-default-features
```

No test launches a browser by default, so the suite needs nothing installed. The
exception is opt-in: `cargo test -p kingdom-browser -- --ignored` launches a
real Chrome. Kingdom finds Chrome itself at runtime — `PATH`, the usual install
locations, or a Chromium that Playwright or Puppeteer already downloaded. Set
`KINGDOM_CHROME_EXECUTABLE` only to override a wrong guess.

Two things that actually bite:

- **`cargo fmt` is edition-sensitive, and this workspace is mixed.**
  `kingdom-citymap` is edition 2024; everything else is 2021. `cargo fmt` picks
  each crate's style edition from its own manifest, so a file formatted by a
  bare `rustfmt` can come out dirty under `cargo fmt`. Always run
  `cargo fmt --all`, never `rustfmt` on a single file.
- **A test must not read the process environment.** `tools::child_environment`
  pins `KINGDOM_MODEL=mock` for a plan working in a Kingdom checkout, so a test
  that reads that variable passes on your machine and fails inside Kingdom.
  `llm::catalogue::default_id` takes the preference as a parameter for exactly
  this reason; do the same rather than reaching for `std::env::var` in a
  decision you want to test.

### What a plan needs that the tests do not

The suite runs on a bare machine; *driving a real project* does not, and the gap
is invisible until a plan is halfway through a job:

- **A browser**, for the `browser_*` tools. Any Chrome or Chromium on `PATH`.
  On arm64, Google Chrome has no Linux build — Chromium is the native one.
- **`taskset`**, to hold that browser to its CPU ceiling. Almost always present
  (`util-linux`); without it a browser still runs, simply unconfined.
- **`slirp4netns`**, but only for a plan the King opens with a network of its
  own. Without it such a plan has no DNS, no crates.io and no git, so Kingdom
  refuses to open one and names the package instead of degrading. Nothing else
  needs it, and the default plan does not.
- **Docker**, but only for a project that declares shared services in
  `<city>/.kingdom/services.toml`, or a King who keeps his own in
  `~/.kingdom/services.toml`. Kingdom starts one container per service and hands
  its address to every plan that can reach it. Without a daemon such a city
  refuses to run rather than starting an agent that cannot reach its own
  database. Almost no project needs this and no test does.
- **Whatever the city itself needs to run.** That is the *project's*
  prerequisite, not Kingdom's, and Kingdom cannot install it.

None is checked up front on purpose — except `slirp4netns`, which *is* checked
before the plan opens, because "you asked for isolation and silently did not get
it" is a worse answer than a refusal. A plan that only reads and proposes needs
none of them, and refusing to start without them would be worse than the
diagnosis.

The browser cannot hand a server a real filesystem path, so the opening screen
asks the King to type one. A native folder picker would require shipping as a
desktop shell (Tauri) — a deliberate later decision, not an oversight.
