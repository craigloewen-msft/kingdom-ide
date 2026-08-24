# Workspaces for a decree, and a prompt free of the metaphor

Two changes that happen to meet in the same place — what a plan is *told* and
what a plan is *pointed at*.

1. **Nothing playful reaches the model.** The metaphor is for the King, not for
   the thing drafting. Today it leaks into the wire in two ways, and one of them
   is worse than it looks.
2. **A decree chooses its workspace.** Fresh worktree, a named branch in its own
   worktree, or the folder itself. This is the first time a plan is pointed at
   something other than the project directory, and it is what makes several
   agents on one repo stop being a collision.

---

## Part 1 — the prompt stops speaking in court

### What leaks today

**The system prompt.** `llm/copilot.rs::system_prompt` opens with *"You are an
architect in Kingdom IDE, advising the King on one project."* Replace with a
plain statement of role and constraints. `CityBrief::render` is already neutral
and stays as it is.

**Kingdom's own notices, replayed as if the model said them.** This is the real
poisoning. `Speaker` has exactly two variants, so every message the *app*
authors is stored as `Speaker::Court`:

- `api.rs` — model failures (`"no credential: …"`)
- `sample.rs` — the opening court's placeholder summaries

and `copilot.rs` maps every `Speaker::Court` turn to `role: "assistant"`. So on
the next turn of a conversation the model is shown Kingdom's plumbing as its own
prior words, and will happily continue in that voice.

### The fix

A notice is **not a third speaker**. Nothing utters it, nothing can reply to it,
and it is never part of the exchange — it is information *about* the chat shown
inside the chat. Modelling it as a `Speaker` variant would mean every match on
speaker has to remember to exclude it, and the first place that forgets is the
place that poisons the prompt again. So the distinction is made one level up, in
the transcript entry itself, where the type system enforces it:

```rust
/// One line of a plan's chat log: either something was said, or something
/// happened.
pub enum Entry {
    /// Words a participant produced. These, and only these, go to a model.
    Said(Utterance),
    /// Something Kingdom itself reports: a failed call or a workspace cut.
    /// Never sent anywhere.
    Note(Note),
}

pub struct Note {
    pub body: String,
    pub kind: NoteKind, // Failed | Workspace
}
```

`Speaker` keeps its two variants and its exact meaning. `Plan.transcript`
becomes `Vec<Entry>`; `plan.say(speaker, body)` stays, and `plan.note(kind,
body)` joins it. Ordering is preserved because notices live in the same vector —
they have to, since where a notice lands in the conversation is part of the
information it carries.

Call sites:

- `Brief.transcript` narrows to `Vec<Utterance>`, built by `plan.said()` — an
  iterator over `Said` entries only. The `llm` layer never sees a `Note` at all,
  so `copilot.rs` cannot send one even by accident, and its `match speaker` stays
  exhaustive over two arms.
- `api.rs` uses `note(...)` for model failures and workspace events; the "last
  King turn is the prompt" search runs over `said()`, so a notice landing between
  turns cannot displace the prompt.
- `sample.rs`'s failed plan carries a `Note`, which is what it always was.
- `conversation.rs` renders a `Note` as a centred muted system line, not a chat
  bubble (`_conversation.scss`). An app notice is not counsel and should not be
  dressed as counsel — this is the visible half of the same correction.
- The mock model's `(Drafted by the mock model — no real work was done.)` tail
  stays: it never reaches a real provider, and it is the marker that tells the
  King no real work happened.

**Test (regression, one):** a plan whose transcript interleaves King turns,
Court turns and notes yields a `Brief` containing the utterances in order and
none of the note text, with the last King turn correctly picked as the prompt.
This is the thing that was silently wrong and would silently regress.

---

## Part 2 — three ways to start a chat

### Domain (`kingdom-core`)

```rust
pub enum WorkspaceMode {
    /// A throwaway worktree cut from the city's current HEAD.
    Fresh,
    /// A named branch, checked out into its own worktree.
    Branch(String),
    /// The project directory itself. No isolation.
    InPlace,
}

pub struct Workspace {
    pub mode: WorkspaceMode,
    /// Absolute path the plan actually reads and writes.
    pub path: String,
    /// The branch checked out there, when there is one.
    pub branch: Option<String>,
    /// The GUID naming the worktree folder, for `Fresh` and `Branch`.
    pub id: Option<String>,
}
```

`Plan` gains `pub workspace: Workspace`, settled when the plan opens and never
changed afterwards — a conversation that silently moved between checkouts would
be a record of nothing in particular, the same reasoning that already pins
`ModelChoice` to the plan.

Pure and wasm-safe, so it lives in `model.rs`. No git, no `std::fs` here.

### Preparing a workspace (`kingdom-app/src/worktree.rs`, ssr only)

`prepare(city_root: &Path, mode: &WorkspaceMode) -> Result<Workspace, WorktreeError>`

- `InPlace` → the city root, no git touched at all.
- `Fresh` → GUID `g`; `git -C <city> worktree add -b kingdom/<g> <city>/.kingdom/<g> HEAD`
- `Branch(b)` → GUID `g`; `git -C <city> worktree add <city>/.kingdom/<g> <b>`
  (existing local or remote-tracking branch; a branch already checked out
  elsewhere is refused by git, and that refusal is surfaced verbatim).

Also:

- `.kingdom/` is appended once to the city's `.git/info/exclude`, so isolation
  does not itself dirty the repo it is isolating. `info/exclude` rather than
  `.gitignore` because the King's repo is not ours to commit to.
- A city with no `.git` asks for `Fresh`/`Branch` → a plain error naming the
  problem ("Fauxville is not a git repository, so it cannot be worked in a
  worktree"), surfaced on the decree bar. It is not a silent downgrade to
  `InPlace`; the King asked for isolation and must know he did not get it.
- `uuid = { version = "1", features = ["v4"] }` added to the `ssr` feature only.

### Coordination

This task originally used the old lease broker to serialize `git worktree add`.
That broker was removed before this branch merged because no runtime path ever
created meaningful contention. Workspace creation now relies on git's own
locking and surfaces any refusal verbatim. Real resource arbitration returns
when plans can run commands or write files, against collisions that actually
exist.

### Server functions (`api.rs`)

- `begin_plan(prompt, city, choice, mode)` — prepares the workspace before the
  plan is created and records it on the plan. It must fail loudly before a plan
  exists rather than leave a plan pointing nowhere. It stays fast — one git
  command — and the model call remains in `draft_plan`.
- `draft_plan` briefs the model with `plan.workspace.path`, not the city path,
  and uses `Plan::working_on` to prevent duplicate model calls.
- New `list_branches(city) -> Vec<String>` (`git for-each-ref`, local branches,
  HEAD first) so the picker offers real branches instead of a free-text box
  that can only be typed wrong.

### UI

- A workspace chip in the decree bar beside the model chip, opening a small
  picker in the same style as `ModelPicker`: **Fresh worktree** / **Branch…** /
  **This folder**. Choosing *Branch* reveals the branch list for the selected
  city. Chip reads e.g. `worktree`, `branch: fix/parser`, `in place`.
- Last mode remembered in `localStorage` next to the model choice; a remembered
  `Branch` whose branch has since gone degrades to `Fresh` rather than failing
  the decree, mirroring how a withdrawn model is handled.
- The chamber header shows which workspace the plan holds, with the full path on
  hover. Isolation the King cannot see is isolation he cannot trust.

**Test (behaviour, one, needs `git`):** in a temp repo, `Fresh` produces a
checkout under `.kingdom/<guid>` on its own branch with the same HEAD commit;
`Branch(b)` produces one with `b` checked out; `InPlace` creates nothing. Two
`Fresh` calls produce two distinct directories.

### Explicitly out of scope

- **Removing worktrees.** They persist under `.kingdom/` so the King can inspect
  or merge them. Pruning is its own decision (which plans are done? what about
  uncommitted work?) and guessing at it now would throw away real work.
- Committing, merging, or pushing anything. A plan still only talks.

---

## Order of work

1. `Entry`/`Note` in core; `say`/`note`/`said` through api, llm, conversation,
   sample + the brief test; neutral system prompt.
2. `Workspace`/`WorkspaceMode` in core; `Plan.workspace`.
3. `worktree.rs` + checkout behavior tests.
4. `begin_plan`/`draft_plan`/`list_branches` rewiring.
5. Decree-bar picker, remembered mode, chamber header, styles.
6. `cargo fmt`, `cargo clippy`, `cargo test`, then drive all three modes in the
   browser against a real repo — the map and chamber are looked at, not just
   compiled.
7. Update `AGENTS.md` §5 ("what is real") to say a plan now has a workspace.
