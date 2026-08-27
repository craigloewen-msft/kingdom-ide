# A database the whole city shares

**Status:** done · **Priority:** p1

Network isolation answered *"stop these agents colliding on a port"*. This
answers the question immediately behind it: some resources are meant to be
**shared**. Five plans on a project that needs MongoDB should reach one MongoDB,
started once and stopped once.

Shown as **the well**; called a shared service in code.

## The measurement the design rests on

Taken from inside a real plan namespace before any code was written:

| Probe | Result |
|---|---|
| namespace → container on the default bridge (`172.17.0.2:5432`) | reachable |
| namespace → container on a custom subnet (`172.31.77.10:27017`) | reachable |
| namespace → host loopback (`127.0.0.1:47777`, a published port) | **refused** |
| host route table after `docker network create --subnet` | `br-* → 172.31.77.0/24` |

`slirp4netns` runs with `--disable-host-loopback`, which blocks `127.0.0.1` and
nothing else. This inverted the obvious design: publishing the container and
pointing plans at `127.0.0.1` is the one path that provably cannot work. Plans
get the container's bridge address instead, and no change to `netns.rs` was
needed at all.

The fourth row killed a feature. The proposal included an in-process tokio TCP
proxy so the King could reach the service from the host; the route Docker
installs makes it reachable already, so the proxy was deleted before it was
written. The King caught this in review and was right.

## What was built

- `kingdom-core/src/services.rs` — the manifest at `<city>/.kingdom/services.toml`,
  parsed and validated. Pure and wasm-safe, so all of it is tested without a
  disk or a daemon.
- `kingdom-app/src/services.rs` — a deliberate sibling of `netns.rs`: a
  process-global registry, shelling out to `docker` rather than taking on
  `bollard`, one network per city with a derived `/24`, addresses assigned from
  manifest order.
- Wiring: `ensure` beside `netns::ensure` in `turn.rs` and `terminal.rs`,
  addresses into `tools::child_environment`, `release` beside `netns::shutdown`,
  the address into the system prompt, and a `SharedService` on the wire for the
  ports badge.
- `shopfront` fixture — one city, one MongoDB, and a **real runnable** Node
  ledger. The first fixture whose files are actual code, because a claim about
  the network cannot be tested with sized filler.

## Three things worth remembering

**The address is assigned, not allocated.** A service's IP comes from its
position in the manifest, so it is knowable before the container exists — which
is what lets it be substituted into `MONGODB_URI` and printed in the badge.
Docker's DNS would not help: it resolves service names only between containers
on the same network, so neither the host nor a plan's namespace can resolve `db`.

**Adopted on restart, not killed.** The one place this deliberately differs from
`netns::reclaim_previous`. A stale namespace is worthless; a stale database
holds state. Stopped rather than removed, named volume kept.

**The git exclude had to change.** `.kingdom/` excluded the *directory*, so git
never looked inside and a manifest committed there was invisible — a later
`!.kingdom/services.toml` has nothing to act on. The working form is
`.kingdom/*` plus the negation, and repositories carrying the old rule are
brought forward. Measured against real `git status`, not inferred, and pinned by
two tests.

## Proven by running it

Against a real Docker daemon (`--ignored`, the rule `kingdom-browser` follows
for Chrome):

- `services_against_real_docker` — start, adopt, share, release, restart, and
  the volume outliving the container.
- `five_agents_share_one_database` — five plans handed one address, counted to
  five; four leaving leaves it up; the fifth stops it.

And by hand against the seeded fixture: five agents wrote to one MongoDB and
`GET /entries` listed all five authors. Every probe container, network and
volume was removed afterwards.

## Not built, on purpose

Runtimes other than Docker; non-network resources (the shared `target/`);
health checks beyond "the port answers"; arbitration between plans *writing* the
same collection. That last one is real — the well makes concurrent writes
possible without making them safe, and the system prompt says so rather than
implying otherwise.

Docker Desktop on macOS keeps the daemon in a VM and does not route to container
IPs, so the King reaching the address directly is a Linux property. Noted rather
than built for.
