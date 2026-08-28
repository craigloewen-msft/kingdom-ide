# Two tabs for isolation, and folders you can untick

**Status:** done · **Priority:** p1

The isolation panel asked one question about two things, opened slowly, and let
the King share a folder but never stop sharing it. It is now a **Network** tab
and a **Files** tab, and the folders under Files are checkboxes.

## What the King sees

| Tab | Options |
|---|---|
| Network | Host network · Its own network |
| Files | Host machine · Own file system, with a checkbox per folder beneath it |

Each tab carries its current answer in the tab itself (`Network its own`), so
both settings are readable with only one open.

## Three decisions worth keeping

**The tabs are a projection, not a second model.** `kingdom_core::Isolation` is
untouched: still one enum, still three rungs, still `#[serde(alias = "network")]`
for the plan records already on disk, still the single `is_isolated()` question
~100 call sites ask. Two tabs suggest four combinations and only three exist,
because the holder is one `unshare` in which `--mount` is *added* to `--net`.

The fourth square is **said rather than hidden**: with Files on "own file
system", the Network tab's host row is disabled and carries the reason. That is
the shape the panel already used for a missing `slirp4netns`, and the argument
is the same — an option that quietly disappears teaches nothing, and a King who
presses something that does nothing learns less still.

The two rows also refuse to undo each other behind his back. "Its own network"
pressed while sealed does *not* drop the filesystem; "Host machine" pressed while
sealed steps down one rung to `Isolated`, not two. Each control changes only the
thing it names.

**The panel no longer asks Docker anything, and this was the real slowness.**
Opening it fired three server calls, one of which — `shared_resources()` — goes
through `services::inventory` to `docker_trouble()`, which shells out to
`docker version` **with no timeout**. So on a machine where Docker is slow,
absent or wedged, the panel sat waiting on a daemon in order to print a footnote
about databases, then changed height when it finally landed. The footnote is
gone; `/resources` is where a well is reviewed. What is left is
`network_available()` (a `PATH` lookup) and `mount_offers()` (a `PATH` walk and
some `stat`s), both fetched when the *panel* opens, so switching tabs shows a
list rather than a spinner. The body has a fixed ceiling and scrolls inside it,
because a panel that grows as answers land moves Start out from under the
cursor.

**A share you cannot undo is a decision nobody revisits.** `declare_mount` had
no counterpart: to stop lending `~/.ssh` you had to find
`~/.kingdom/services.toml` and edit TOML. `services::withdraw_mount` is the
inverse, and edits the text for the reason `declare_mount` appends text — a
serde round trip would eat every comment in the file as the price of removing
one block.

Four things that had to be right for a checkbox to be honest:

- **The block is found by parsing, not by matching the path as a string.**
  `path = "~/.ssh"` and `path = '~/.ssh'` are one folder written two ways, and a
  string match leaves one of them behind — which reads as a box that unticks and
  then re-ticks itself.
- **A block ends before the comment above the next one.** The naive
  header-to-header rule takes a trailing comment with the block above it, where
  a person reading the file sees a note about the folder *below*. Removing
  `~/.cargo` would have deleted the King's note about `~/.ssh`.
- **`MountCandidate` carries the scope it was declared at, not a bool.** A box
  may only be unticked where unticking does something, and this panel writes to
  his profile alone. A folder the *project* declared is shown ticked and fixed:
  it is part of what the plan sees, and its manifest is committed and somebody
  else's too.
- **Half a toolchain is not shared.** An offer counts as ticked only when every
  folder in it is declared — `~/.cargo` without `~/.rustup` is a `cargo` that
  re-downloads the toolchain, and a box ticked for it would be a promise the
  mount cannot keep.

The list also now includes folders declared *by hand*, and shows a declared
folder even when it no longer exists — the exact opposite of the rule for an
offer, and deliberate: an offer Kingdom would silently skip is worse than none,
while a stale line in a manifest is the one most worth clearing.

## What is not done

**None of it was compiled.** The plan that carried out this work was opened
without `~/.cargo` or `~/.rustup` shared, so `cargo` did not exist inside it:
`fmt`, `clippy`, the four test suites and the wasm check were all left to the
King. The irony is the feature's own — this is precisely the folder-sharing the
Files tab exists to make easy.
