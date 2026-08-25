# Commands that need no `cd`

Every bash call the court makes opens with `cd <workspace root> && …`. The
command already starts there. The prefix is pure waste, it is paid on every
round for the rest of the plan, and the tool's own description is what teaches
the habit.

This is a wording change in two strings, plus tests to pin them. No behaviour
changes.

## The evidence

Counted across the plan records on this machine (`~/dev/.kingdom/plans/*.json`,
ten plans with real turns):

| | |
|---|---|
| `bash` calls | 365 |
| beginning with `cd` | 260 (71%) |
| …`cd`-ing to the plan's **own workspace root** | **233 (64% of all calls)** |
| `cd`-ing somewhere genuinely else | 27 |

The 27 are legitimate — `/tmp`, a sibling project, a probe directory — and must
keep working. The 233 are no-ops.

The cost is not the ~80 bytes it takes to emit one. It is that the transcript is
resent on every round, so a prefix written in round 3 is paid again in every
round after it:

| plan | redundant prefixes | bytes emitted | bytes *replayed* |
|---|---|---|---|
| `plan-11` | 69 | 5,658 | ~428,000 (~107k tokens) |
| `plan-10` | 44 | 3,608 | ~147,000 (~37k tokens) |

That is the same recurring-cost argument `MOST_GUIDANCE` is built on, arriving
by a different door.

## Why the model does it

Three strings, and the loudest one points the wrong way.

| Where | Says | Reads as |
|---|---|---|
| `tools/bash.rs:105` — second line of the description | "Bash state changes (**working dir**, variables, aliases) don't persist between calls." | *your `cd` will be lost, so re-establish it each time* |
| `tools/bash.rs:174` — the `cmd` parameter | "Runs under `bash -c` with the workspace as its working directory." | correct — and buried in a parameter description, which many gateways render after the prose |
| `llm/system_prompt.rs:239` — `workspace_block` | "Working directory: `<path>`" | states *where*, never states that commands **start** there |

Nothing anywhere says "you do not need to `cd`". The most prominent sentence
implies you do. The habit is a rational response to what the court is told.

## Changes

### 1. `crates/kingdom-app/src/tools/bash.rs` — the description's opening

The first two lines become:

```
Executes shell commands via bash -c, capturing combined stdout/stderr.
Every call starts in your workspace root, so you never need to `cd` there first.
Bash state changes (working dir, variables, aliases) don't persist between
calls — each call starts fresh at that root.
```

Two things about this, both deliberate:

- The new sentence goes **before** the persistence line, so the affordance is
  read first and the non-persistence line lands as its explanation rather than
  as an instruction to compensate.
- The persistence line keeps its meaning and gains the clause that makes it
  useful. Losing your `cd` matters; where you land when you lose it is the part
  that was missing.

### 2. `crates/kingdom-app/src/tools/bash.rs` — the `cmd` parameter

> "The shell command, for op=run. Runs under `bash -c` in your workspace root."

Same fact, shorter, and no longer the only place it is stated.

### 3. `crates/kingdom-app/src/llm/system_prompt.rs` — `workspace_block`

The first line gains a second clause:

```
Working directory: <path>
Every command runs here, and every relative path is resolved from here.
```

The isolation sentences that follow are untouched. This is the block's existing
job — grounding the court where it stands — finished properly.

### 4. `crates/kingdom-app/src/tools/tmux.rs` — the `cmd` parameter

> "The command, run via `bash -lc` in your workspace root."

Aligned for consistency, but deliberately **not** given the "never `cd`"
sentence: a tmux pane is a live shell whose working directory *does* persist, so
the claim would be false there. Worth stating in the commit rather than leaving
as an apparent oversight.

## On departing from Phoenix's wording

`bash.rs`'s description is Phoenix's verbatim, and AGENTS.md §4 is explicit that
the port keeps Phoenix's wording unless Kingdom's *facts* differ. They do not
here — Phoenix's commands also start in the working directory. So this is the
`SHARED_MACHINE` case rather than the `label`/`since` case: an **addition**
Kingdom has its own measured reason for, kept alongside Phoenix's sentence
instead of replacing it.

The module comment above the description already explains why the negations in
it are load-bearing. It gains a short paragraph recording this addition and the
numbers above, so the next person to diff against Phoenix finds the reason
rather than a mystery divergence.

## Tests

Both are wording pins, in the style of `the_court_is_warned_about_the_shared_machine`.

- **`tools/bash.rs`** — `the_court_is_told_it_is_already_in_the_workspace`:
  asserts the description contains the no-`cd` sentence, and that it appears
  *before* the "don't persist" line. The ordering is the half that does the work,
  so it is the half worth pinning.
- **`llm/system_prompt.rs`** — `the_workspace_block_says_commands_start_there`:
  asserts a rendered prompt contains both the path and the "every command runs
  here" clause, under an isolated workspace and an in-place one alike.

No existing test asserts the old wording, so nothing needs unpinning. Verify
with:

```bash
cargo test -p kingdom-app --features ssr --no-default-features
```

## What this does not do

- **No stripping of a redundant `cd`.** The transcript must show what the model
  actually sent; rewriting a command before running it makes the chamber a
  record of something that did not happen.
- **No in-band correction on the tool result.** Considered and set aside: plain
  wording has never had a fair run while the description contradicted it. If the
  rate does not fall, the `<next_step>` precedent in `patch.rs` is the shape to
  reach for next, and this task's numbers are the baseline to measure against.
- **No change to `Sandbox::root` or to what `bash` may reach.** A command that
  wants `/tmp` still `cd`s to `/tmp`.

## Verifying it worked

Re-run the count against plans created after the change:

```bash
python3 - <<'EOF'
import json, glob, re
tot = cdws = 0
for f in glob.glob('/home/omarchy/dev/.kingdom/plans/*.json'):
    d = json.load(open(f)); ws = (d.get('workspace') or {}).get('path')
    def walk(o):
        global tot, cdws
        if isinstance(o, dict):
            i = o.get('input')
            if o.get('tool') == 'bash' and isinstance(i, dict) and isinstance(i.get('cmd'), str):
                tot += 1
                m = re.match(r'\s*cd\s+([^\s;&]+)', i['cmd'])
                if m and ws and m.group(1).strip('\'"').rstrip('/') == ws.rstrip('/'):
                    cdws += 1
            for v in o.values(): walk(v)
        elif isinstance(o, list):
            for v in o: walk(v)
    walk(d)
print(f"{cdws}/{tot} bash calls cd to their own workspace ({cdws/tot:.0%})")
EOF
```

Baseline today: **64%**. Anything in single digits is the change working.
