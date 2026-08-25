# Port Phoenix IDE's system prompt and agent setup into Kingdom

Kingdom's agents answer worse than Phoenix's on the same work. This ports
Phoenix's prompt construction, its wire settings, its tool descriptions and its
skills machinery across, and rips out the Kingdom-specific machinery they
replace.

**Phoenix's implementation is the reference and the arbiter.** Where Kingdom has
invented something Phoenix does not have, the default is to delete it, not to
preserve it. Kingdom's doc comments argue for several of these inventions at
length; those arguments do not override this task.

Reference: `~/dev/phoenix-ide`, chiefly `crates/phoenix-ide/src/system_prompt.rs`,
`crates/phoenix-core/src/llm_language.rs` (the `PhoenixNative` arms),
`crates/phoenix-tools/src/*.rs`, `crates/phoenix-skills/`, and
`crates/phoenix-llm/src/openai.rs`.

## Why

Four differences, in descending order of suspected impact.

**Assembly order.** Phoenix ends the prompt with the mode block — the
instruction saying what the agent may do *right now* is the last thing it reads.
Kingdom puts its permissions block early, then follows it with up to 64 KB of
`AGENTS.md`, so the remit is the least recent thing in a very long prompt.

**Wire settings.** `phoenix-llm/src/openai.rs` sends `parallel_tool_calls: true`
and `tool_choice: "auto"` whenever tools are present, with a comment explaining
it saves (N-1) round-trips. `kingdom-app/src/llm/copilot.rs::request_body` sends
neither. Kingdom is likely getting one tool call per round where Phoenix gets a
batch — which costs latency *and* coherence, because every extra round re-sends
the whole transcript.

**Tool descriptions.** Phoenix's are the ecosystem-standard phrasings models
have strong priors on. Kingdom rewrote every one into house prose.

**Skills.** Phoenix discovers `.claude/skills/` and `.agents/skills/`, injects an
`<available_skills>` catalogue into the prompt, and exposes a `skill` tool.
Kingdom has none of this.

## 1. Rewrite `crates/kingdom-app/src/llm/system_prompt.rs` as a literal port

Replace `SystemPrompt::render` with Phoenix's
`build_system_prompt_with_options` order:

```
base prompt  (Phoenix's exact wording, llm_language.rs::base_prompt)
mermaid hint (Phoenix's exact wording)
<project_guidance> …
<available_skills> …
worktree grounding note
MODE BLOCK          ← last
sub-agent suffix    ← only for a subagent
```

Delete `PREAMBLE`, `ENDING_A_TURN`, `ECONOMY`, `TESTING` and `SCREENSHOTS`, and
the test `every_acting_remit_is_told_that_prose_ends_the_turn` that pins the
second of them. Phoenix sends none of these; nothing replaces them.

Map `Permissions` onto Phoenix's `ModeContext` blocks, porting the
`LlmLanguage::PhoenixNative` text verbatim and substituting Kingdom's nouns
(`propose_plan` for `propose_task`, plan for task):

| Kingdom | Phoenix block |
|---|---|
| `ReadOnly` | `sub_agent_suffix`, adapted — Kingdom subagents return prose, they have no `submit_result` tool |
| `Propose` | `mode_explore` |
| `Full` | `mode_work` |
| `Full` + approved | `mode_work` + the carrying-out sentence |

`mode_explore` names a tasks directory and a `patch` allowlist scoped to it.
Kingdom has neither — a proposing plan calls `propose_plan` with a title and a
body, and has no `patch` at all. Port the block's shape and its wording wherever
it transfers; drop the taskmd-filename paragraph and the `next_taskmd_id_hint`
rather than inventing a Kingdom equivalent.

Keep `workspace_block`. It is Kingdom's version of the worktree grounding note
Phoenix emits from `repo_root_from_phoenix_worktree`, so it is a port, not an
invention.

**Guidance discovery matches Phoenix.** Phoenix's `discover_guidance_files`
walks to the filesystem root; Kingdom's stops at the kingdom root. Change
Kingdom's to Phoenix's behaviour and delete the
`guidance_above_the_kingdom_is_left_alone` test that pins the old bound. Keep
the content-hash dedup — Phoenix has the same thing, for the same worktree
reason.

## 2. Drop the city brief from the prompt

`CityBrief::render` puts up to 40 file paths into every system prompt, on every
round of a loop that may run 500 times. Phoenix sends none. Stop calling
`self.city.render()` in `SystemPrompt::render`.

Keep the `CityBrief` type and `notable_paths`: `llm/mock.rs` reads
`city.notable_paths` and `city.name` to build its offline replies. Keep the
struct, stop rendering it.

## 3. `parallel_tool_calls` and `tool_choice` on the wire

In `copilot.rs::request_body`, when `!tools.is_empty()`, also send:

```rust
body["tool_choice"] = json!("auto");
body["parallel_tool_calls"] = json!(true);
```

Both stay absent when there are no tools, for the reason the existing comment
above the `tools` array already gives.

Kingdom already handles batched calls end to end — `Acts.calls` is a `Vec`, and
`copilot.rs::messages` regroups calls sharing a `ToolCall::batch` back into one
assistant message — so this needs no executor change. Verify against the
existing batching test in `copilot.rs` rather than assuming.

## 4. Terse ecosystem tool descriptions

Replace `description()` in each of these with Phoenix's text from the matching
file in `crates/phoenix-tools/src/`, verbatim:

`think`, `read_file`, `search`, `bash`, `patch`, `ask_user_question`,
`spawn_agents`, `read_image`, the `browser_*` tools, `profile`.

This includes `read_image`'s "saved to a temp file but not automatically
visible" framing. Phoenix renders screenshots inline in its conversation too
(`ui/src/components/MessageComponents.tsx` handles `browser_take_screenshot`
display data), so the description is no less accurate in Kingdom than it is in
Phoenix. Port it as-is.

For `propose_plan`, adapt `propose_task`'s description: same framing ("the
gateway from Explore mode to Work mode", "must be the only tool call in the
response"), but Kingdom takes a title and a body inline rather than a file path.

Schemas stay as they are; this is a description-only pass.

## 5. `skill` tool + skills catalogue

Port `crates/phoenix-skills/src/lib.rs` discovery into Kingdom (a module under
`kingdom-app`, ssr-only — it walks the filesystem and must not reach the wasm
bundle):

- scan `.claude/skills/` and `.agents/skills/` walking up from the workspace,
  plus `$HOME`
- parse `name` / `description` / `argument-hint` from `SKILL.md` frontmatter
- namespaced sub-skills (`allium/skills/distill` → `allium:distill`)
- symlink, content and name dedup, nearest-wins

Then the `<available_skills>` prompt block (Phoenix's exact wording, including
"Do not cat SKILL.md files directly") and a `skill` tool that returns the
frontmatter-stripped body prefixed with `Base directory for this skill: …`, with
`$ARGUMENTS` substitution.

**Not porting:** `phoenix-skills/src/builtin.rs`. Phoenix embeds built-in skills
via `rust-embed` and extracts them to `~/.phoenix-ide/builtin-skills/`. Kingdom
has no built-ins to embed, so `SkillSource` collapses to the filesystem variant
and `display_location` always renders a path.

`skill` sits with the reads in `tools::all` — invoking one returns text and
changes nothing, so it is available at every permission level.

## 6. Rip out the `NUDGE` machinery in `api.rs`

Phoenix has no equivalent: a reply with prose and no tool call simply ends the
turn. Kingdom instead detects a narration-only first reply, writes a
`NoteKind::Failed` note, appends an instruction to the brief and sends the model
back round. That is the runtime half of the same invention `ENDING_A_TURN` was
the prose half of, and it goes with it.

Delete from `crates/kingdom-app/src/api.rs`:

- the `NUDGE` and `NUDGED` constants
- `nudge_next`, the `nudged` transcript scan, and the `has_acted` computation
  that exists only to feed it
- the `if !has_acted && !nudged { … continue; }` arm in `Reply::Spoke`, so
  `Spoke` goes straight to `settle`
- the block in the turn loop that appends `NUDGE` to
  `brief.system_prompt.permissions`

That last deletion also removes the only mutation of an assembled
`SystemPrompt` outside `system_prompt.rs`. Once it is gone the `let mut brief =
brief;` rebinding is dead — drop it and build the `Brief` once.

## 7. Match Phoenix's subagent limits

Kingdom's caps were invented, not ported, and both are tighter than Phoenix's:

| | Kingdom | Phoenix |
|---|---|---|
| subagents per call | `MOST_SUBAGENTS = 6` | `maxItems: 10` |
| rounds per subagent | `MOST_SUBAGENT_ROUNDS = 12` | 20 (explore default) |

Raise both to Phoenix's numbers and replace the comments justifying the old ones
(Kingdom's cites gateway rate limits; Phoenix does not find them binding).

Also port Phoenix's `max_turns` per-task field, letting the model choose a lower
bound for a cheap question, with 20 as the default.

## Not in scope

- **The `LlmLanguage` switch and the Caveman variants.** Port the
  `PhoenixNative` text as plain constants; Kingdom has no per-conversation
  language setting and adding one is a separate decision.
- **Work-mode subagents and named `.claude/agents/` personas.** Explicitly
  excluded when this task was scoped.
- **Widening what a subagent may hold.** Kingdom's subagents get
  `think`/`read_file`/`search`/`read_image`. Phoenix's *explore* subagents get
  all of those plus every `browser_*` tool, `keyword_search`, and a sandboxed
  `bash` — so Kingdom is stricter than Phoenix here, and this task does not
  close that gap. Two separate reasons, worth keeping apart:
  - the browser half is portable and is simply not in this task's scope; it is
    the obvious follow-up.
  - the bash half is **not** portable as-is. Phoenix's is `SandboxedBashTool`
    under an OS-enforced sandbox. Kingdom has no such thing — `Sandbox::root` is
    explicit that its path boundary does not contain a shell — so handing
    Kingdom's subagents plain `bash` would give several concurrent agents
    unrestricted write access to one worktree. That is more than Phoenix does,
    not less, and it needs an OS sandbox first.
- **`keyword_search`**, which needs a model call of its own.

## Acceptance

- `SystemPrompt::render` emits Phoenix's block order, mode block last.
- `PREAMBLE`, `ENDING_A_TURN`, `ECONOMY`, `TESTING`, `SCREENSHOTS`, `NUDGE` and
  `NUDGED` are gone, along with the tests pinning them.
- Guidance discovery walks to the filesystem root, matching Phoenix; content
  dedup retained.
- No file listing in the prompt; `CityBrief` still compiles and `mock.rs` still
  names real files in its replies.
- `request_body` sends `tool_choice` and `parallel_tool_calls` with tools, and
  neither without; a test pins both directions.
- Tool descriptions match Phoenix's verbatim.
- Skills discovered from both directories with dedup and nearest-wins;
  `<available_skills>` appears only when at least one is found; `skill` returns
  a stripped body with the base directory prefixed. Tests cover discovery order,
  dedup, and the empty case.
- Subagent caps match Phoenix: 10 per call, 20 rounds, `max_turns` accepted
  per task.
- `AGENTS.md` updated: `skill` moves out of §4's "tools the court does not
  have"; the `Propose` narrative in §4 no longer describes `ENDING_A_TURN`-style
  prompt guidance or the nudge; §3's `llm/` description matches the new
  assembly order.
- `cargo test -p kingdom-core` and
  `cargo test -p kingdom-app --features ssr --no-default-features` green.
