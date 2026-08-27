# A ledger of the wells: seeing and creating shared resources

**Status:** done · **Priority:** p1

Shared resources were real but almost invisible. A city declared containers in
`<city>/.kingdom/services.toml`, `services.rs` started one set for it, and the
only place the King ever saw them was a popover behind the plug icon *inside a
plan's chamber*. So he could not see what his machine shared without opening a
plan, could not declare one without knowing the TOML by heart, and a manifest
with a typo in it was silent until an agent's first turn was refused — minutes
in, with a message about the model.

This adds the screen, and the one missing level.

## What was built

- **`/resources`** (`components/wells.rs`) — every declared resource grouped by
  who owns it, and a detail pane carrying the address, the plans using it *by
  title*, the environment as an agent actually receives it, the container name
  for `docker logs`, and the **absolute path of the file it is declared in**.
  Reached from the cities rail and from the ports badge, which gained a
  scope pill and a link here.
- **A host level.** `~/.kingdom/services.toml`, beside `settings.json`,
  offered to every project in every kingdom. `services::Scope` is the whole
  seam: everything downstream — network, container name, subnet, reference
  count — was already a function of one key string.
- **A form that writes TOML**, previewing the exact block first.
- **`docs/shared-resources.md`** — every field, both levels, the placeholders,
  why never `localhost`, and how to diagnose one.

## Three things worth remembering

**The scope cost a type, not a rewrite.** `ensure`/`release`/`environment` were
already keyed on `(key, name)` where the key was a city's. Host wells file under
`host` — a string no city key can collide with, since a city key always carries
a `-<8 hex>` suffix. A host well's user set therefore spans *cities*, so it is
released when the last plan **anywhere** lets go. Proven against a real daemon:
`a_host_well_serves_two_projects` raises one Redis from a plan in project A,
adopts it from a plan in project B at the same address, and stops it only when
both are done.

**An empty `Vec` does not survive a server function's argument encoding.** The
form failed with `invalid type: string "", expected a sequence` for the most
ordinary input there is — a Redis with no environment. Caught by driving a real
browser against a real server, not by the type checker, and not by any test
written before it. The fix sends the `KEY=value` **text** and parses it in
`kingdom-core`, which is better anyway: one representation, shared by the
preview and the writer, in the shape the file itself has.

**`ManifestError` could no longer name the file.** It hardcoded
`.kingdom/services.toml` in every message — fine with one manifest, wrong half
the time with two: a broken profile reported a path inside a project that had
nothing wrong with it. `kingdom-core` does no I/O and cannot know which file it
was handed, so the path moved to `services::manifest_in`, the only layer that
does. A test pins that the error carries *no* path and another that the ledger's
message contains the real one.

## Decisions worth keeping

**The form appends text; it never re-serialises the document.** The manifests
are hand-written and commented — the `shopfront` fixture opens with a paragraph
explaining what Kingdom does with the file — and a round trip through `toml`
would eat every one of those comments as the price of adding a service. Pinned
by a test that declares a second service into a commented file and asserts the
comment survives.

**The screen reports state; it never commands it.** No start or stop buttons. A
well is raised when a plan needs it and stopped when the last plan lets go; a
stop button would fight that reference count in front of five working agents.

**Removal is done by editing the file.** Which is why the path is the single
most prominent thing in the detail pane.

**Docker is asked one question for the whole screen**, not one per row: a daemon
that is down becomes one banner rather than a column of confusing "not
started"s, and a declared-but-idle resource is honestly reported as `unknown`
rather than guessed at.

## Proven by running it

Unit: 21 in `kingdom-core::services` (render/parse round trips, escaping, the
scopes, `parse_env`), 9 more in `kingdom-app::services` (the two files, the
reserved key, the writer, duplicate refusal, the ledger over both scopes and a
broken manifest).

`--ignored`, against real Docker 29.7.2: `a_host_well_serves_two_projects`.

By hand, driving a real Chromium against a server on the `shopfront` realm:
declared a host-level Redis with and without environment, saw it land in the
profile with the project's manifest untouched, saw the ledger regroup, hit the
duplicate refusal, broke the profile manifest on purpose and watched the row
appear naming the right file. Two of the bugs above were found exactly here and
nowhere else.
