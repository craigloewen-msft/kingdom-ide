# A machine of a plan's own, and three choices instead of a tab

**Status:** done · **Priority:** p1

The decree bar's isolation panel was a tab strip with one tab in it, and the one
tab did one thing: give a plan its own network. That answered half of the
product's second question. A plan with its own `:3000` still had the King's
whole disk and his own uid — `netns.rs` said so plainly in its own docs — so an
agent that went wrong could not take a port from anyone and could still delete
his home directory.

This replaces the strip with **three exclusive choices**, and adds the third.

## What the King sees

| Shown as | In code | What it gets |
|---|---|---|
| On this machine | `Isolation::Shared` | Nothing of its own. Still the default |
| A network of its own | `Isolation::Isolated` | Exactly what it got before |
| A machine of its own | `Isolation::Sealed` | That, plus its own filesystem and process table |

Under the third, a **quick-add** list of folders to allow in, built from his own
`PATH`. One press writes a `[[mount]]` block to his profile.

The visualisation is deliberately bare — a list that works, so the decision can
be made now; what a plan can reach deserves a real picture, and that is its own
piece of design.

## Three decisions worth keeping

**The ladder, not the menu.** `Isolation` is one enum whose rungs each include
the one below, and `is_isolated()` answers true for both isolated kinds. That is
why adding a third mode touched **no** call site among the ~100 that ask it: the
turn loop, the terminal, `bash`, `tmux`, the ports badge, the map and
`AgentNetwork` all mean "is there a namespace to enter" and a sealed plan
answers that exactly as an isolated one does. A flag set would have made every
one of them think about a combination nothing wants.

**One holder, several namespaces — measured, not preferred.** The King asked
whether the module could split into a `netns` and a `mount`. It could, and did,
*by responsibility*; it could not by holder **process**. Two separately created
user namespaces are siblings, and an unprivileged process may not enter a
sibling user namespace: two holders work only when the server happens to run as
root, and fail elsewhere by attaching the network and silently not the mounts.
Kingdom must not be quietly more capable as root. `netns.rs` became
`namespaces/{mod,net,mount}.rs` — a pure move first, with the suite green, so a
breakage would be attributable.

**Quick-add reads `PATH`, because that is the only honest answer.** Not a list
somebody wrote: `PATH` is the list his own shell uses. A recognised entry brings
the folders its tool *needs* — `~/.cargo` without `~/.rustup` is a `cargo` that
re-downloads the toolchain, watched happening from inside a real namespace — and
an unrecognised one is still offered, because a tool Kingdom has never heard of
is still a tool he has.

## Seven traps, none of which would have looked like an error

The first five were measured by hand before any of this was written; the last
two were found by running it.

1. **`nsenter --wdns`, not `--wd` and not `current_dir`.** Every caller sets the
   working directory host-side, and it is resolved *before* the mount namespace
   is entered. A sealed plan ran every command in `/`, silently.
2. **tmux's `#{pid}` is namespace-local.** The staleness check read it and
   `readlink /proc/<pid>/ns/net` on the host — which fails, or names an
   unrelated process. Every failure path returned "this daemon is fine". The
   server is stamped with its holder instead.
3. **The tmux socket lives under `/tmp`,** which a sealed plan makes private, so
   the daemon was invisible to the 14 host-side calls that drive it.
4. **`/etc/resolv.conf` is a symlink to somewhere unmounted** on both WSL and
   systemd-resolved machines: DNS failed while the network was perfectly up.
5. **`/bin` is a symlink on every current distribution.** Mounting it would
   mount `/usr/bin` twice under two names; the symlinks are reproduced instead.
6. **The private `/tmp` was mounted after the binds beneath it,** hiding a
   workspace under `/tmp` — which is exactly where a test workspace lives.
7. **`/proc` was never created in the new root.** `pivot_root` succeeded, the
   next line failed, the holder died, and it surfaced a second later as
   `nsenter: cannot open /proc/<pid>/ns/user` — naming nothing that was wrong.

An eighth was caught by the live test and by nothing else: `net::create` called
`MountPlan::built_in`, which ignores declared folders, so **every mount the King
declared was silently dropped**. The namespace came up perfectly and `cargo`
simply was not in it. There is now a two-line unit test that would have caught
it, beside the live one that did.

A ninth came from *looking at the machine* after a live run rather than reading
anything: each sealed plan left its scratch root under `$XDG_RUNTIME_DIR`, one
per plan ever sealed, for the life of the machine. `tear_down` sweeps it now,
and the live test asserts it is gone.

## On the King's open question: the user

He asked whether to include the user, and worried about root. The answer is that
the user namespace is **not optional** — it is what lets an unprivileged process
mount and `pivot_root` at all — and that `uid=0` inside is *mapped*, not
granted: what a plan can touch is decided by the mounts. Measured, a write to
`/usr/bin` from inside is refused.

Worth recording because it changes the shape of the question: on the machine
this was built on, the Kingdom server **already runs as root**. So "add the
user" and "don't" were the same uid, and the mounts were doing all the work.
That is an argument for the feature rather than against it.

## What is not done

- **Chrome in a mount namespace is unproven.** It was flagged as unproven in the
  proposal and remains so. `browser_*` in a sealed plan is the most likely thing
  to need a second pass.
- **A sealed plan with a shared service** is untested end to end. The relay path
  is unchanged and enters `--user`/`--net` only, so it should work; expecting is
  not measuring.
- The visualisation, as agreed.

## Proving it

The ordinary suite runs on a bare machine, as AGENTS.md requires. What needs a
real kernel is opt-in:

```bash
cargo test -p kingdom-app --features ssr --no-default-features -- --ignored live::
```

Three live tests: a sealed plan's filesystem, process table and working
directory; a network-only plan still seeing the machine; and `cargo --version`
running inside a sealed namespace off the King's own declared `~/.cargo` and
`~/.rustup`. That last one is the claim the whole feature rests on.
