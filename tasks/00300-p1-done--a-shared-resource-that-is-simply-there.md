# A shared resource that is simply there, at its own port

**Status:** done · **Priority:** p1

The shared-resources screen and its "new resource" form described a mechanism
instead of a promise. The form's central field asked the King to write
`DATABASE_URL = "postgres://{host}:{port}/app"`, and both the ledger and the
ports badge printed a container IP. But the infrastructure for the promise was
already built — `netns::open_wells` relays each container onto an isolated
plan's own loopback — so an agent could already say `mongodb://localhost:27017`
and be right. Nothing said so.

This makes the promise true everywhere it was not, prints it where the King
looks, and deletes the environment variable outright.

## What a declaration is now

```toml
[[service]]
name   = "db"
image  = "mongo:7"
port   = 27017
volume = "shopfront-db"
```

Four fields. `env`, `parse_env`, `ServiceSpec::environment`, the `{host}`/
`{port}` substitution, `UnknownPlaceholder`, `SharedResource::environment`,
`services::environment` and `tools::service_environment` are all gone. Nothing
is injected into a plan's commands for a shared resource any more.

The form is image, name, port, volume — and the port and volume fill themselves
in from the image.

## Three bugs, two of which no test would have caught

**A second resource on the same port was handed the first one's data.**
`open_wells` recorded relayed *ports*, and the address decision asked "is this
port relayed?". Two resources on `:6379` — the King's own Redis and a project's,
which is the ordinary shape of the host/project split — produced one relay, and
*both* were told `localhost:6379`. Not an error: a wrong database. A well now
records the container it reaches, and `services::address_for` matches on that.

**`postgres:16` — the form's own placeholder — could not start.** Kingdom passed
the container no environment, and Postgres exits 1 without `POSTGRES_PASSWORD`.
All the King saw was "never answered on port 5432". `kingdom_core::known_image`
now carries each well-known image's port, data directory and boot variables;
`data_dir_for` folded into it so there is one table rather than two.

**The system prompt never mentioned the database at all.** `SystemPrompt::assemble`
passed `CityBrief::path` to `services_block` — but that is the plan's
*workspace*, a worktree under `.kingdom/`, while a shared resource is filed
under the *city's* key. The lookup matched nothing and the block came out empty.
Pre-existing, and survivable only while `$MONGODB_URI` was also set; the moment
the prompt became the only channel it was the whole feature silently missing.
`assemble` now takes the city root explicitly.

That last one was found by **reading a real plan's prompt in a browser**, not by
any test — the same way the last two bugs in this area were found. Two tests now
pin it, and I watched both fail with the fix reverted.

## Decisions worth keeping

**A manifest still carrying `env` is refused by name, not ignored.** Serde drops
an unknown key silently, which would leave a project believing it sets
`$DATABASE_URL` while nothing does — a failure that surfaces an hour later as a
bug in the project's own code. `ServiceSpec::retired_env` exists solely to be
rejected, and `RetiredField` deserialises from anything and keeps none of it.

**No `container_env` field.** Adding a second env-shaped field while deleting
the first is the complexity the King objected to. An image outside the table
that needs variables simply fails to answer on its port, which the ledger
already reports by name with `docker logs`.

**`LOOPBACK` became `localhost`, not `127.0.0.1`.** Every consumer is now prose
— a prompt, a badge, a screen. The old spelling existed because the value was
substituted into a connection string; that substitution is gone.

**Plan defaults were not changed.** `localhost` is a property of a plan with a
network of its own, because a relay on the machine's network would bind the
King's real port. Instead the isolation picker now names what the selected
project shares and the address it would be reached at, so the choice is informed
rather than silent.

**The volume is named by default** (`kingdom-<scope>-<name>-data`). Losing a
database because an optional box was left empty is the worse of the two
mistakes. Clearing the box still means "data goes with the container" — the form
distinguishes "untouched" from "deliberately emptied".

## What it cost

A plan on the machine's network no longer gets an automatic `$MONGODB_URI`; it
reads the address from its prompt, which still names it. That falls on the mode
this feature is not written around. An existing manifest with `env` in it needs
one line deleted, and says so.

## Proven by running it

Unit: 109 in `kingdom-core` (the retired-field refusal, the image table, the
four-field round trip), 337 in `kingdom-app` — including two resources on one
port getting different addresses, in both `services::address_for` and the
prompt's own words, and the worktree/city-root regression in two places.

`--ignored`, against real Docker 29.7.2: `services_against_real_docker`, extended
with a `postgres:16` declared exactly as the form writes one — image, name, port
and nothing else. It boots and answers only because the boot table reaches
`docker run`.

By hand, driving Chromium against a server on the seeded `shopfront` realm:
declared a Postgres through the new form (port and volume auto-filled, preview
four fields, comment in the manifest survived), saw both rows read
`localhost:<port>`, broke the manifest with an `env` block and watched the
refusal name the file and the fix, opened a plan with a network of its own, and
**completed a real Postgres protocol handshake over `localhost:5432` from inside
that plan's namespace** while the King's own `127.0.0.1:5432` stayed refused.
