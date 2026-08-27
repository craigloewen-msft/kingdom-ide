# A network of a plan's own

**Status:** done.

Each plan may be opened with its own Linux network namespace, so two agents can
both bind `:3000` without colliding. A terminal button and a ports badge in the
chamber header are the two ways the King reaches into one.

## Why

AGENTS.md names the collision as the reason this product exists: "Two bind port
3000." Until now Kingdom could only watch it happen. This is the first real
answer to question 2 for one resource.

## What was built

- `kingdom-core`: `NetworkMode` (`Shared` / `Isolated`) and `PortForward`, plus
  `Plan::network` and `Plan::ports`. A **separate axis** from `WorkspaceMode`,
  because folder isolation and port isolation are independent wants.
- `kingdom-app/src/netns.rs`: the namespace per plan, the slirp4netns process
  that gives it a way out, port discovery, and forwarding.
- `kingdom-app/src/terminal.rs` + `components/terminal_view.rs`: a real pty over
  a socket, drawn with a vendored xterm.js.
- `components/ports_badge.rs`: what is listening, and the host URL for it.
- `components/prompt_bar.rs`: the network chip and picker, remembered in
  `localStorage`, with the isolated row *disabled and explained* when
  slirp4netns is missing.
- Call sites: `tools/bash.rs`, `tools/tmux.rs`, `kingdom-browser`'s Chrome, and
  the terminal all prepend `netns::enter_prefix`.

## Decisions worth keeping

**Off by default.** The King asks for it per plan. A machine without
slirp4netns is told what to install rather than handed a degraded namespace with
no DNS — that would break every `cargo build` and look like Kingdom's fault.

**`enter_prefix` is empty for a shared plan.** Every call site prepends it
unconditionally. A call site that had to *check* is one that will forget, and
the one that forgets starts a server on the King's own port.

**Chrome had to move too.** This was a correctness bug, not a nicety: a host
Chrome told to open `localhost:3000` would reach another plan's server and
screenshot the wrong project while reporting success. `kingdom-browser` gets an
`on_enter_namespace` hook rather than a dependency on `netns`, and launches
through a small `exec` wrapper because chromiumoxide takes an executable rather
than an argv.

**No bridge process.** slirp4netns forwards ports itself over a JSON API
socket (`add_hostfwd` / `remove_hostfwd`), so the planned bridge binary and the
`socat` dependency both disappeared.

## Two bugs found by running it

**A silent fall back to the host network.** After a server restart the
process-global registry is empty while plan records still say `Isolated`, so
`enter_prefix` returned nothing and the terminal opened a shell on the *host*
network — while its own header said "in this plan's network". It tried to bind
`:3000` and took `EADDRINUSE` from the King's real server. `terminal.rs` now
calls `ensure` and **refuses** rather than degrading. This is the exact failure
mode the design set out to prevent, committed by the implementation.

**A blank terminal panel.** `js_sys::Function::new_with_args("stage, url", ..)`
compiles the body with those names already declared, so a body that also read
`const stage = arguments[0]` was a `SyntaxError` at construction — swallowed by
a `let _`. Now `new_no_args`, matching `markdown.rs`, and the call's error is
reported to the console instead of discarded.

## Verified end to end

Against a real kernel and a real slirp4netns, in the browser:

- an isolated plan bound `:3000` while the King's own `:3000` kept serving;
- the badge showed `3000 -> 127.0.0.1:47983` and that URL returned the page;
- stopping the server withdrew the forward within a second and cleared the badge;
- `curl https://index.crates.io/config.json` returned 200 and `git ls-remote`
  a real SHA from inside the namespace;
- the isolated shell **could not** reach the host's `:3000` (`000`) while a
  shared-network shell could (`200`), each reporting a different
  `/proc/self/ns/net`;
- a shared plan created no namespace at all and behaved exactly as before;
- restarting the server reclaimed the previous one's holder and slirp.

## Known limits

- **Not a security boundary.** A process in the namespace still has the whole
  filesystem and the King's uid. Collision avoidance only, and the docs say so.
- **TCP only.** UDP is not forwarded; nothing needed it yet.
- **Linux only**, as asked — enforced at *runtime* rather than by `cfg`.
  `availability()` refuses on any other platform, so the module still compiles
  everywhere and there is one place that decides. The `/proc` reads and
  `unshare`/`nsenter` calls inside it are Linux-shaped and would simply never be
  reached elsewhere.
- Plans cannot reach *each other's* networks, by construction. If two plans ever
  need to talk, that is a new decision rather than a missing feature.
