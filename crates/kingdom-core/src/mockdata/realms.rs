//! **The fake data. Edit this file to change it.**
//!
//! Each realm is a function returning a [`RealmSpec`]. To add one, write the
//! function and list it in [`realms()`] -- that is the whole procedure. The
//! helpers in [`super::build`] (`rust_city`, `file`, `fill`, `text`, ...) are
//! what keep a realm readable; see [`super`] for the full walkthrough.
//!
//! Two things to keep in mind when editing:
//!
//! - **Seeds are arbitrary but must not change casually.** Changing a realm's
//!   seed reshuffles every generated file size in it, which moves every tower on
//!   the map. That is fine when intended and confusing when not.
//! - **Each realm should earn its place** by making some state reachable that
//!   the others do not. They are fixtures, not a gallery.

use super::build::*;
use super::{CitySpec, RealmSpec};
use crate::model::Language;

/// The realm the "Enter the Proving Grounds" button opens.
pub const DEFAULT_REALM: &str = "kingdom-mirror";

// Per-realm seeds. Arbitrary values -- what matters is that they are *fixed*.
// Changing one reshuffles every generated file size in that realm, which moves
// every tower on its map; fine when intended, baffling when not.
const MIRROR_SEED: u64 = 0x_D1FF_0001;
const CROWDED_SEED: u64 = 0x_C0FF_0002;
const MONOREPO_SEED: u64 = 0x_BEEF_0003;

/// Every realm the seeder can build. **Add yours here.**
pub fn realms() -> Vec<RealmSpec> {
    vec![kingdom_mirror(), crowded(), monorepo()]
}

/// Looks up a realm by name.
pub fn realm(name: &str) -> Option<RealmSpec> {
    realms().into_iter().find(|r| r.name == name)
}

/// Every realm name, for the CLI's listing and its unknown-name error.
pub fn realm_names() -> Vec<&'static str> {
    realms().into_iter().map(|r| r.name).collect()
}

// ---------------------------------------------------------------------------

/// A fake dev folder shaped like a real one: mixed stacks, mixed sizes.
///
/// The everyday proving ground, and what the button opens. Deliberately
/// *modest* in size so it seeds in well under a second -- the realms that
/// stress-test the scanner are separate, because a slow default would push
/// people back towards opening their real folder.
fn kingdom_mirror() -> RealmSpec {
    RealmSpec::new(
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
            .dir("tests", [fill("case_{i}.rs", 6, 800..3_000, Language::Rust)])
            .dir("docs", [file("design.md", 6_400), file("api.md", 3_100)])
            .dirty(3),
        node_city("lantern")
            .dir(
                "src",
                [
                    file("index.ts", 2_400),
                    fill("component_{i}.tsx", 32, 900..8_000, Language::Web),
                    dir("styles", [fill("_{i}.scss", 8, 400..2_500, Language::Style)]),
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
            .dir("tests", [fill("test_{i}.py", 9, 600..2_800, Language::Python)]),
        docs_city("chronicle")
            .dir("notes", [fill("{i}-entry.md", 40, 800..9_000, Language::Docs)])
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
/// *here* rather than on the King's machine. Each city is tiny; the point is the
/// count, not the bulk.
fn crowded() -> RealmSpec {
    const NAMES: [&str; 40] = [
        "alder", "birch", "cedar", "dogwood", "elm", "fir", "gorse", "hazel", "ivy", "juniper",
        "kapok", "larch", "maple", "nutmeg", "oak", "pine", "quince", "rowan", "spruce", "teak",
        "umber", "vine", "willow", "xylem", "yew", "zelkova", "ash", "beech", "chestnut", "date",
        "ebony", "fig", "ginkgo", "holly", "iron", "jarrah", "karri", "linden", "mahogany",
        "nyssa",
    ];

    let cities = NAMES.iter().enumerate().map(|(i, name)| {
        // Sizes fan out by a factor of ~50 across the realm, which is what makes
        // the map's size scaling and label thresholds meaningfully exercised.
        let count = 2 + (i * 3) % 60;
        let city: CitySpec = match i % 4 {
            0 => rust_city(name).dir("src", [fill("mod_{i}.rs", count, 400..20_000, Language::Rust)]),
            1 => node_city(name).dir("src", [fill("part_{i}.ts", count, 300..15_000, Language::Web)]),
            2 => python_city(name).dir(
                "pkg",
                [fill("unit_{i}.py", count, 300..12_000, Language::Python)],
            ),
            _ => go_city(name).dir("cmd", [fill("step_{i}.go", count, 400..10_000, Language::Go)]),
        };
        if i % 5 == 0 {
            city.dirty(i % 7)
        } else {
            city
        }
    });

    RealmSpec::new(
        "crowded",
        "Forty cities. For map layout, labels and level-of-detail.",
        CROWDED_SEED,
    )
    .cities(cities)
}

/// One enormous project, nested well past the scanner's depth cap.
///
/// Drives every limit in `scan.rs` at once: `SCAN_DEPTH`, the `COUNT_CAP`
/// budget, `FILES_PER_DISTRICT` pruning into `extra_files`/`extra_bytes`, and the
/// assets-versus-code weighting. Those caps are invisible until something
/// crosses them, and a real monorepo is a bad place to discover they misbehave.
fn monorepo() -> RealmSpec {
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

    RealmSpec::new(
        "monorepo",
        "One vast project: depth caps, file caps, and a huge asset.",
        MONOREPO_SEED,
    )
    .city(
        node_city("leviathan")
            .files([deep])
            .dir("src", [fill("core_{i}.ts", 240, 800..40_000, Language::Web)])
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
