//! **The fake data. Edit this file to change it.**
//!
//! Each fixture is a function returning a [`FixtureSpec`]. To add one, write
//! the function and list it in [`fixtures()`] -- that is the whole procedure.
//! The helpers in [`super::build`] (`rust_city`, `file`, `fill`, `text`, ...)
//! are what keep a fixture readable; see [`super`] for the full walkthrough.
//!
//! Two things to keep in mind when editing:
//!
//! - **Seeds are arbitrary but must not change casually.** Changing a
//!   fixture's seed reshuffles every generated file size in it, which moves
//!   every tower on the map. That is fine when intended and confusing when not.
//! - **Each fixture should earn its place** by making some state reachable that
//!   the others do not. They are fixtures, not a gallery.

use super::build::*;
use super::{CitySpec, FixtureSpec};
use crate::model::{CityKind, Language};

/// The fixture the "Enter the Proving Grounds" button opens.
pub const DEFAULT_FIXTURE: &str = "kingdom-mirror";

// Per-fixture seeds. Arbitrary values -- what matters is that they are *fixed*.
// Changing one reshuffles every generated file size in that fixture, which
// moves every tower on its map; fine when intended, baffling when not.
const MIRROR_SEED: u64 = 0x_D1FF_0001;
const CROWDED_SEED: u64 = 0x_C0FF_0002;
const MONOREPO_SEED: u64 = 0x_BEEF_0003;
const SHOPFRONT_SEED: u64 = 0x_5EED_0004;

/// Every fixture the seeder can build. **Add yours here.**
pub fn fixtures() -> Vec<FixtureSpec> {
    vec![kingdom_mirror(), crowded(), monorepo(), shopfront()]
}

/// Looks up a fixture by name.
pub fn fixture(name: &str) -> Option<FixtureSpec> {
    fixtures().into_iter().find(|r| r.name == name)
}

/// Every fixture name, for the CLI's listing and its unknown-name error.
pub fn fixture_names() -> Vec<&'static str> {
    fixtures().into_iter().map(|r| r.name).collect()
}

// ---------------------------------------------------------------------------

/// A fake dev folder shaped like a real one: mixed stacks, mixed sizes.
///
/// The everyday proving ground, and what the button opens. Deliberately
/// *modest* in size so it seeds in well under a second -- the fixtures that
/// stress-test the scanner are separate, because a slow default would push
/// people back towards opening their real folder.
fn kingdom_mirror() -> FixtureSpec {
    FixtureSpec::new(
        "kingdom-mirror",
        "Five projects, mixed stacks -- the everyday proving ground.",
        MIRROR_SEED,
    )
    .cities([
        rust_city("orchard")
            .dir(
                "src",
                [
                    file("main.rs", 4_200),
                    file("lib.rs", 9_800),
                    fill("module_{i}.rs", 24, 1_500..12_000, Language::Rust),
                ],
            )
            .dir(
                "tests",
                [fill("case_{i}.rs", 6, 800..3_000, Language::Rust)],
            )
            .dir("docs", [file("design.md", 6_400), file("api.md", 3_100)])
            .dirty(3),
        node_city("lantern")
            .dir(
                "src",
                [
                    file("index.ts", 2_400),
                    fill("component_{i}.tsx", 32, 900..8_000, Language::Web),
                    dir(
                        "styles",
                        [fill("_{i}.scss", 8, 400..2_500, Language::Style)],
                    ),
                ],
            )
            .dir(
                "public",
                [file("logo.svg", 14_000), file("hero.png", 320_000)],
            )
            .dirty(1),
        python_city("almanac")
            .dir(
                "almanac",
                [
                    file("__init__.py", 320),
                    fill("task_{i}.py", 18, 1_200..7_000, Language::Python),
                ],
            )
            .dir(
                "tests",
                [fill("test_{i}.py", 9, 600..2_800, Language::Python)],
            ),
        docs_city("chronicle")
            .dir(
                "notes",
                [fill("{i}-entry.md", 40, 800..9_000, Language::Docs)],
            )
            .dir("assets", [file("diagram.excalidraw", 88_000)]),
        // No git, so `has_git: false` is reachable -- it changes what the map
        // draws, and would otherwise never be seen in development.
        rust_city("forge")
            .dir(
                "src",
                [
                    file("main.rs", 1_800),
                    fill("pass_{i}.rs", 7, 700..4_000, Language::Rust),
                ],
            )
            .no_git(),
    ])
}

/// Forty cities of wildly varying size.
///
/// Exists so map layout, label collision and level-of-detail switching fail
/// *here* rather than on the user's machine. Each city is tiny; the point is
/// the count, not the bulk.
fn crowded() -> FixtureSpec {
    const NAMES: [&str; 40] = [
        "alder", "birch", "cedar", "dogwood", "elm", "fir", "gorse", "hazel", "ivy", "juniper",
        "kapok", "larch", "maple", "nutmeg", "oak", "pine", "quince", "rowan", "spruce", "teak",
        "umber", "vine", "willow", "xylem", "yew", "zelkova", "ash", "beech", "chestnut", "date",
        "ebony", "fig", "ginkgo", "holly", "iron", "jarrah", "karri", "linden", "mahogany",
        "nyssa",
    ];

    let cities = NAMES.iter().enumerate().map(|(i, name)| {
        // Sizes fan out by a factor of ~50 across the fixture, which is what
        // makes the map's size scaling and label thresholds meaningfully
        // exercised.
        let count = 2 + (i * 3) % 60;
        let city: CitySpec = match i % 4 {
            0 => rust_city(name).dir(
                "src",
                [fill("mod_{i}.rs", count, 400..20_000, Language::Rust)],
            ),
            1 => node_city(name).dir(
                "src",
                [fill("part_{i}.ts", count, 300..15_000, Language::Web)],
            ),
            2 => python_city(name).dir(
                "pkg",
                [fill("unit_{i}.py", count, 300..12_000, Language::Python)],
            ),
            _ => go_city(name).dir(
                "cmd",
                [fill("step_{i}.go", count, 400..10_000, Language::Go)],
            ),
        };
        if i % 5 == 0 {
            city.dirty(i % 7)
        } else {
            city
        }
    });

    FixtureSpec::new(
        "crowded",
        "Forty cities. For map layout, labels and level-of-detail.",
        CROWDED_SEED,
    )
    .cities(cities)
}

/// One enormous project, nested well past the scanner's depth cap.
///
/// Drives every limit in `scan.rs` at once: `SCAN_DEPTH`, the `COUNT_CAP`
/// budget, `FILES_PER_DISTRICT` pruning into `extra_files`/`extra_bytes`, and
/// the assets-versus-code weighting. Those caps are invisible until something
/// crosses them, and a real monorepo is a bad place to discover they misbehave.
fn monorepo() -> FixtureSpec {
    let deep = dir(
        "packages",
        (0..8).map(|p| {
            dir(
                format!("pkg-{p}"),
                [
                    file("package.json", 900),
                    dir(
                        "src",
                        [
                            fill("unit_{i}.ts", 90, 500..14_000, Language::Web),
                            // Past SCAN_DEPTH from the city root: the scanner
                            // must stop here and still report honestly.
                            dir(
                                "internal",
                                [dir(
                                    "deep",
                                    [dir(
                                        "deeper",
                                        [fill("buried_{i}.ts", 20, 400..3_000, Language::Web)],
                                    )],
                                )],
                            ),
                        ],
                    ),
                ],
            )
        }),
    );

    FixtureSpec::new(
        "monorepo",
        "One vast project: depth caps, file caps, and a huge asset.",
        MONOREPO_SEED,
    )
    .city(
        node_city("leviathan")
            .files([deep])
            .dir(
                "src",
                [fill("core_{i}.ts", 240, 800..40_000, Language::Web)],
            )
            .dir(
                "assets",
                [
                    // 40 MB, written sparsely so it costs almost nothing on
                    // disk. This is the exact shape behind the tested "assets
                    // never outweigh code" invariant: if weighting regresses,
                    // this single file buries the entire source tree.
                    file("trailer.mp4", 40 * 1024 * 1024),
                    file("poster.png", 2 * 1024 * 1024),
                ],
            )
            .dirty(12),
    )
}

/// One project, one database, and room for five agents to prove it is shared.
///
/// Every other fixture is *shaped* like a project. This one **runs**: the files
/// are real Node, the manifest is a real manifest, and `npm install && node
/// server.js` actually serves. That is deliberate and it is the whole point --
/// the claim being tested is that five agents reach one MongoDB, and a fixture
/// of sized filler cannot test a claim about the network.
///
/// It earns its place by reaching a state no other fixture can:
///
/// - a city with a `.kingdom/services.toml`, so the well is exercised at all;
/// - five plans on one city, each binding `:3000` in its own namespace, which
///   crosses network isolation with shared services in the one place where they
///   could plausibly fight;
/// - **shared mutable state**, so "another agent wrote this" is visible rather
///   than argued about.
fn shopfront() -> FixtureSpec {
    // The manifest. A kind, four fields and no environment: an agent reaches
    // this database at `localhost:27017`, which is what `server.js` below
    // connects to with nothing configured.
    let manifest = "\
# What this project needs standing in order to run.
#
# Kingdom starts these once for the whole project, shares them between every
# agent working on it, and stops them when the last one is done. Each agent
# reaches this database at `localhost:27017` -- its own loopback, spliced
# through to the one shared container -- so there is nothing to configure.

[[service]]
# What kind of thing this is. `docker` is the only kind today, and the only
# one a manifest written before there were kinds could have meant -- so this
# line is optional and every older file still reads exactly as it did.
type  = \"docker\"
name  = \"db\"
image = \"mongo:7\"
port  = 27017
# A named volume, so the data outlives the container.
volume = \"shopfront-db\"
";

    // One dependency, so `npm install` is quick enough that doing it five times
    // is not the reason the rehearsal fails.
    let package = "\
{
  \"name\": \"shopfront\",
  \"version\": \"0.1.0\",
  \"private\": true,
  \"type\": \"module\",
  \"scripts\": {
    \"start\": \"node server.js\"
  },
  \"dependencies\": {
    \"mongodb\": \"^6.3.0\"
  }
}
";

    // The ledger. Every agent runs this on :3000 -- in its own namespace, so
    // five of them do not collide -- and all five write to one collection.
    let server = r#"// The shared ledger.
//
// Every agent working on this project runs its own copy of this server, on its
// own :3000, in its own network namespace. They all write to the SAME MongoDB,
// which Kingdom started once for the whole project.
//
// That is the thing worth seeing: five servers, five ports, one database.
import { createServer } from "node:http";
import { MongoClient } from "mongodb";

// Kingdom puts the shared database on this plan's own localhost, at MongoDB's
// usual port. Nothing to configure and nothing to read from the environment.
const uri = "mongodb://localhost:27017";

// Who is writing. Each agent should set this to something recognisable so the
// ledger shows whose entry is whose.
const agent = process.env.AGENT_NAME || `agent-${process.pid}`;

const client = new MongoClient(uri);
await client.connect();
const entries = client.db("shopfront").collection("entries");

const server = createServer(async (request, response) => {
  const json = (code, body) => {
    response.writeHead(code, { "content-type": "application/json" });
    response.end(JSON.stringify(body, null, 2));
  };

  try {
    // Write one entry, tagged with whichever agent is serving.
    if (request.method === "POST" && request.url === "/entry") {
      const chunks = [];
      for await (const chunk of request) chunks.push(chunk);
      const raw = Buffer.concat(chunks).toString() || "{}";
      const { note } = JSON.parse(raw);

      const entry = { agent, note: note ?? "(no note)", at: new Date() };
      await entries.insertOne(entry);
      return json(201, entry);
    }

    // Read everything back -- including what the OTHER agents wrote.
    if (request.method === "GET" && request.url === "/entries") {
      const all = await entries.find().sort({ at: 1 }).toArray();
      const authors = [...new Set(all.map((e) => e.agent))];
      return json(200, { servedBy: agent, authors, count: all.length, entries: all });
    }

    if (request.method === "GET" && request.url === "/") {
      return json(200, {
        servedBy: agent,
        database: uri.replace(/\/\/.*@/, "//"),
        try: ["POST /entry {\"note\":\"...\"}", "GET /entries"],
      });
    }

    json(404, { error: "not found" });
  } catch (error) {
    json(500, { error: String(error) });
  }
});

server.listen(3000, () => {
  console.log(`${agent} serving on :3000, writing to the shared ledger`);
});
"#;

    let readme = r#"# shopfront

A **real, runnable** project in the Proving Grounds. Unlike the other fixtures,
the code here works: it is here to prove that several agents share one database.

## What it is

A tiny HTTP ledger backed by MongoDB. `POST /entry` writes a note tagged with
whichever agent wrote it; `GET /entries` reads back everything -- including the
entries the other agents wrote.

## What Kingdom does for you

`.kingdom/services.toml` declares the MongoDB this project needs. Kingdom:

- starts **one** container for the whole project, when the first plan needs it;
- relays it onto **your own `localhost`**, at its usual port, so
  `mongodb://localhost:27017` works here with nothing to set up and nothing to
  read from the environment;
- shows the container's own address in the ports badge, so you can connect to
  it from your machine too;
- stops it when the **last** plan working on this project is done, keeping the
  data in a named volume.

You do not run `docker` yourself. `localhost` is right *because* each plan has
a network of its own: your loopback is nobody else's, so five agents can all
use the same address and all reach the same database.

## The five-agent rehearsal

1. Open **five plans** on this project, each with a network of its own.
2. In each one: `npm install`, then `AGENT_NAME=agent-1 npm start` (numbering
   each agent differently).
3. Every plan binds `:3000` and none of them collide -- each is in its own
   namespace. The ports badge gives each one a different host address.
4. In each plan: `curl -X POST localhost:3000/entry -d '{"note":"hello"}'`.
   That `localhost` is the plan's *own* server -- and `localhost:27017` is that
   plan's route to the one shared database. Both are the plan's own loopback,
   and neither collides with anybody.
5. Then `curl localhost:3000/entries` in any one of them. It lists entries from
   **all five agents** -- one database behind five servers.
6. Open the ports badge: one well, five plans using it.
7. Close four plans. The database stays up. Close the fifth and it stops, with
   the data kept.
"#;

    FixtureSpec::new(
        "shopfront",
        "One project, one shared MongoDB, five agents -- and it really runs.",
        SHOPFRONT_SEED,
    )
    .city(
        // `CitySpec::new` rather than `node_city`, which would write its own
        // placeholder `package.json` and `README.md` on top of the real ones
        // below. `CityKind::Node` is still what the scanner will infer from the
        // `package.json` this fixture does write, so the stack is arrived at
        // exactly as it is for every other city.
        CitySpec::new("shopfront", CityKind::Node)
            .files([
                text("package.json", package),
                text("server.js", server),
                text("README.md", readme),
            ])
            // The manifest, in the project's own `.kingdom/`. It is committed --
            // `worktree::exclude_worktree_dir` re-includes exactly this path,
            // because a bare `.kingdom/` exclude would hide it from git.
            .dir(".kingdom", [text("services.toml", manifest)]),
    )
}
