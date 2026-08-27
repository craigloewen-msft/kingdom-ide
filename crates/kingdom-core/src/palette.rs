//! Which agent did this: a colour per plan, in two values.
//!
//! The third colour axis in Kingdom, and the reason it needs one of its own is
//! that the other two are already spoken for. [`crate::PlanStatus::color`] says
//! *what an agent is doing* -- drafting, failed, merged -- and
//! [`crate::Language::tint`] says *what the code is*. Neither can say **who is
//! touching this file**, which is the first of the three questions in
//! `AGENTS.md` and the one that only becomes urgent when several agents share a
//! town.
//!
//! # Why two colours per agent rather than one
//!
//! A file is not "touched"; it is grown and cut, usually both at once. Before
//! this, growth was one green and cutting one red for every agent alike, so a
//! map with three agents on it could say what happened and not who did it.
//!
//! Giving each agent a *hue* and each direction a *value* is what lets both be
//! read in one glance: hue answers "who", light-or-dark answers "added or
//! removed". The alternative -- a colour per agent and a separate mark for
//! direction -- costs a second glance for the thing the King reads most often.
//!
//! # Why the hues are what they are
//!
//! Chosen by a search rather than by eye, against three constraints measured as
//! weighted-RGB distances (the test below pins all three):
//!
//! | Constraint | Why | Margin achieved |
//! |---|---|---|
//! | Agents apart from each other | two agents on one house must not read as one | 88 |
//! | Growth apart from its own cutting | added and removed must not be confusable | 270 |
//! | Everything apart from the status palette | an agent's colour must never read as "failed" or "merged" | 89 |
//!
//! Separation from [`crate::Language::tint`] is deliberately *not* a hard
//! constraint. A language tint paints a building's face and an agent colour
//! paints a column standing on its roof, so the two are never adjacent surfaces
//! competing to mean the same thing -- and demanding distance from eleven more
//! colours would have cost more than it bought. The nearest approach is 49,
//! which the test records so that a future change to either palette is a test
//! failure rather than a silent collision.

use crate::ids::PlanId;
use serde::{Deserialize, Serialize};

/// An sRGB triple, as the map wants it. Mirrors `kingdom_citymap::map::MapColor`
/// without depending on it -- this crate is the one that knows nothing about a
/// renderer.
pub type Rgb = [u8; 3];

/// One agent's banner: a hue, in the two values the works are drawn with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentPalette {
    /// What the King would call this colour, for a tooltip.
    pub name: &'static str,
    /// Lines added, as CSS.
    pub growth: &'static str,
    /// Lines added, as sRGB for the map.
    pub growth_rgb: Rgb,
    /// Lines removed, as CSS: the same hue, deepened.
    pub cutting: &'static str,
    /// Lines removed, as sRGB for the map.
    pub cutting_rgb: Rgb,
}

/// The ring of banners, in the order a hue search left them.
///
/// Eight because that is comfortably more agents than one person steers in one
/// project at one time, and because past eight the hues stop being tellable
/// apart at the size a map draws them. A ninth concurrent agent reuses a
/// banner rather than inventing an unreadable one -- see [`assign_banners`].
pub const BANNERS: [AgentPalette; 8] = [
    AgentPalette {
        name: "ember",
        growth: "#f5995c",
        growth_rgb: [0xf5, 0x99, 0x5c],
        cutting: "#7f3f15",
        cutting_rgb: [0x7f, 0x3f, 0x15],
    },
    AgentPalette {
        name: "saffron",
        growth: "#f5de5c",
        growth_rgb: [0xf5, 0xde, 0x5c],
        cutting: "#7f6f15",
        cutting_rgb: [0x7f, 0x6f, 0x15],
    },
    AgentPalette {
        name: "moss",
        growth: "#a8f55c",
        growth_rgb: [0xa8, 0xf5, 0x5c],
        cutting: "#4a7f15",
        cutting_rgb: [0x4a, 0x7f, 0x15],
    },
    AgentPalette {
        name: "jade",
        growth: "#5cf5a8",
        growth_rgb: [0x5c, 0xf5, 0xa8],
        cutting: "#157f4a",
        cutting_rgb: [0x15, 0x7f, 0x4a],
    },
    AgentPalette {
        name: "azure",
        growth: "#5cedf5",
        growth_rgb: [0x5c, 0xed, 0xf5],
        cutting: "#157a7f",
        cutting_rgb: [0x15, 0x7a, 0x7f],
    },
    AgentPalette {
        name: "indigo",
        growth: "#5c6bf5",
        growth_rgb: [0x5c, 0x6b, 0xf5],
        cutting: "#151f7f",
        cutting_rgb: [0x15, 0x1f, 0x7f],
    },
    AgentPalette {
        name: "orchid",
        growth: "#f55ced",
        growth_rgb: [0xf5, 0x5c, 0xed],
        cutting: "#7f157a",
        cutting_rgb: [0x7f, 0x15, 0x7a],
    },
    AgentPalette {
        name: "rose",
        growth: "#f55c82",
        growth_rgb: [0xf5, 0x5c, 0x82],
        cutting: "#7f152f",
        cutting_rgb: [0x7f, 0x15, 0x2f],
    },
];

/// A stable number for a plan id.
///
/// FNV-1a, which is what `kingdom_citymap::build::layout::stable_hash` gives a
/// holding its variation from and what `map::works` seeds a ghost house with.
/// The same function in a third place, deliberately: what matters is that the
/// answer never moves between runs, and every one of those three wants that for
/// the same reason.
fn stable_hash(text: &str) -> u32 {
    text.bytes().fold(2_166_136_261u32, |hash, byte| {
        hash.wrapping_mul(16_777_619) ^ byte as u32
    })
}

/// The banner a plan would take if nothing else were competing for it.
///
/// Public because a plan's own chamber and the rail draw this without knowing
/// which other plans are live -- and for a lone plan it is the whole answer.
pub fn preferred(plan: &PlanId) -> &'static AgentPalette {
    &BANNERS[stable_hash(plan.as_str()) as usize % BANNERS.len()]
}

/// Hands every plan in a set a banner, with no two alike while there are
/// banners left.
///
/// # Why this is not simply a hash
///
/// A hash alone keeps a colour stable, which is what makes the rail a usable
/// key -- an agent the King learned was blue an hour ago is still blue. But two
/// plans in one city hashing to the same slot would be drawn identically, and
/// two agents on one map that cannot be told apart is precisely the failure
/// this whole feature exists to fix. Stability is worth a great deal and
/// distinctness is worth more.
///
/// So the hash is a *preference*: each plan takes its preferred banner if it is
/// free, and otherwise walks the ring to the next one that is. The common case
/// -- a handful of plans, no collision -- is pure hashing and perfectly stable;
/// a collision costs only the later plan its preference, and only for as long
/// as both are live.
///
/// # Why the order of `plans` is the caller's business
///
/// Which plan cedes its preference is decided by position, so the caller must
/// hand plans in a stable order or a colour could swap between two agents from
/// one refetch to the next. Callers sort by [`PlanId`], which is what
/// `kingdom_app::api::city_changes` does.
///
/// Beyond [`BANNERS`]`.len()` plans the ring is reused rather than exhausted:
/// nine agents in one town is a case worth drawing badly rather than not at
/// all.
pub fn assign_banners(plans: &[PlanId]) -> Vec<(PlanId, &'static AgentPalette)> {
    let mut taken = [false; BANNERS.len()];
    let mut out = Vec::with_capacity(plans.len());

    for plan in plans {
        let wanted = stable_hash(plan.as_str()) as usize % BANNERS.len();
        // The preferred slot, else the next free one going round the ring.
        let slot = (0..BANNERS.len())
            .map(|step| (wanted + step) % BANNERS.len())
            .find(|slot| !taken[*slot])
            // Every banner is spoken for: more agents than colours, so the
            // ring is reused. Two agents sharing a hue is worse than one
            // agent missing from the map, which is the only alternative.
            .unwrap_or(wanted);
        taken[slot] = true;
        out.push((plan.clone(), &BANNERS[slot]));
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Weighted RGB distance -- the cheap approximation of perceived difference
    /// that the hue search itself was run against. Not a colour science claim:
    /// it is one consistent ruler, which is all a regression test needs.
    fn distance(a: Rgb, b: Rgb) -> f64 {
        let (ar, ag, ab) = (a[0] as f64, a[1] as f64, a[2] as f64);
        let (br, bg, bb) = (b[0] as f64, b[1] as f64, b[2] as f64);
        let mean = (ar + br) / 2.0 / 255.0;
        let (dr, dg, db) = (ar - br, ag - bg, ab - bb);
        ((2.0 + mean) * dr * dr + 4.0 * dg * dg + (3.0 - mean) * db * db).sqrt()
    }

    fn parse(hex: &str) -> Rgb {
        let hex = hex.trim_start_matches('#');
        [
            u8::from_str_radix(&hex[0..2], 16).unwrap(),
            u8::from_str_radix(&hex[2..4], 16).unwrap(),
            u8::from_str_radix(&hex[4..6], 16).unwrap(),
        ]
    }

    /// The CSS spelling and the map spelling of one colour must be one colour.
    /// Two spellings is how a rail and a map come to disagree -- the same trap
    /// `$statuses` and `PlanStatus::color` are held to.
    #[test]
    fn both_spellings_of_a_banner_are_the_same_colour() {
        for banner in BANNERS {
            assert_eq!(parse(banner.growth), banner.growth_rgb, "{}", banner.name);
            assert_eq!(parse(banner.cutting), banner.cutting_rgb, "{}", banner.name);
        }
    }

    /// Two agents on one map that cannot be told apart is the failure this
    /// feature exists to fix, so the hues have to be genuinely far apart --
    /// in both values, since a stack shows cutting bands beside each other too.
    #[test]
    fn no_two_agents_are_confusable() {
        for (i, a) in BANNERS.iter().enumerate() {
            for b in BANNERS.iter().skip(i + 1) {
                assert!(
                    distance(a.growth_rgb, b.growth_rgb) > 70.0,
                    "{} and {} are too close in growth",
                    a.name,
                    b.name
                );
                assert!(
                    distance(a.cutting_rgb, b.cutting_rgb) > 70.0,
                    "{} and {} are too close in cutting",
                    a.name,
                    b.name
                );
            }
        }
    }

    /// Added and removed are opposite facts and must never be confusable, which
    /// is what the value split buys.
    #[test]
    fn what_an_agent_adds_never_looks_like_what_it_cuts() {
        for banner in BANNERS {
            assert!(
                distance(banner.growth_rgb, banner.cutting_rgb) > 200.0,
                "{} does not separate growth from cutting",
                banner.name
            );
        }
    }

    /// The constraint that earns this its own axis. An agent colour that read
    /// as `Failed` orange or `Merged` blue would say something about the plan's
    /// *state*, which it knows nothing about.
    #[test]
    fn no_banner_reads_as_a_plan_status() {
        let statuses = [
            crate::PlanStatus::Drafting,
            crate::PlanStatus::AwaitingReview,
            crate::PlanStatus::Failed,
            crate::PlanStatus::Merged,
            crate::PlanStatus::Archived,
        ];
        for banner in BANNERS {
            for status in statuses {
                let status_rgb = parse(status.color());
                for (label, colour) in [
                    ("growth", banner.growth_rgb),
                    ("cutting", banner.cutting_rgb),
                ] {
                    assert!(
                        distance(colour, status_rgb) > 70.0,
                        "{}'s {label} reads as {:?}",
                        banner.name,
                        status
                    );
                }
            }
        }
    }

    /// Not a hard constraint -- see the module docs for why a language tint and
    /// an agent colour are never adjacent surfaces -- but pinned so that moving
    /// either palette toward the other is a test failure rather than a silent
    /// collision.
    #[test]
    fn the_nearest_language_tint_is_no_nearer_than_it_was() {
        let languages = [
            crate::Language::Rust,
            crate::Language::Web,
            crate::Language::Python,
            crate::Language::Go,
            crate::Language::Systems,
            crate::Language::Shell,
            crate::Language::Markup,
            crate::Language::Style,
            crate::Language::Config,
            crate::Language::Docs,
            crate::Language::Other,
        ];
        let nearest = BANNERS
            .iter()
            .flat_map(|b| [b.growth_rgb, b.cutting_rgb])
            .flat_map(|colour| {
                languages
                    .iter()
                    .map(move |l| distance(colour, parse(l.tint())))
            })
            .fold(f64::MAX, f64::min);
        assert!(
            nearest > 45.0,
            "an agent colour has drifted onto a language tint: {nearest}"
        );
    }

    /// The property that makes the rail a usable key: an agent the King learned
    /// was jade is still jade after a reload, a restart, or a second tab.
    #[test]
    fn a_plan_keeps_its_colour() {
        let plan = PlanId::new("plan-42");
        assert_eq!(preferred(&plan), preferred(&plan));
        // And through `assign`, which is what actually draws it.
        let once = assign_banners(&[plan.clone()]);
        let again = assign_banners(&[plan.clone()]);
        assert_eq!(once, again);
        assert_eq!(once[0].1, preferred(&plan));
    }

    /// The property that matters more. Whatever the ids, a set of live plans
    /// must come back with no two colours alike -- this is the whole feature.
    #[test]
    fn concurrent_agents_are_always_told_apart() {
        for count in 1..=BANNERS.len() {
            let plans: Vec<PlanId> = (0..count)
                .map(|n| PlanId::new(format!("plan-{n}")))
                .collect();
            let assigned = assign_banners(&plans);
            assert_eq!(assigned.len(), count);
            for (i, (_, a)) in assigned.iter().enumerate() {
                for (_, b) in assigned.iter().skip(i + 1) {
                    assert_ne!(a.name, b.name, "two of {count} agents share a banner");
                }
            }
        }
    }

    /// Two plans that genuinely want the same slot: the first keeps it, the
    /// second is bumped rather than drawn identically. Found by construction --
    /// the ids are searched for until they collide.
    #[test]
    fn a_collision_costs_the_later_plan_its_preference_and_nothing_more() {
        let first = PlanId::new("plan-1");
        let wanted = preferred(&first);
        let clash = (2..2_000)
            .map(|n| PlanId::new(format!("plan-{n}")))
            .find(|id| preferred(id) == wanted)
            .expect("some id in two thousand shares a slot with plan-1");

        let assigned = assign_banners(&[first.clone(), clash.clone()]);
        assert_eq!(assigned[0].1, wanted, "the first keeps its preference");
        assert_ne!(assigned[1].1, wanted, "the second must not be identical");
        // Alone, the bumped plan still gets the colour it always had.
        assert_eq!(assign_banners(&[clash.clone()])[0].1, wanted);
    }

    /// More agents than colours is a case worth drawing badly rather than not
    /// at all: everyone is still given a banner.
    #[test]
    fn more_agents_than_banners_still_all_get_one() {
        let plans: Vec<PlanId> = (0..BANNERS.len() + 3)
            .map(|n| PlanId::new(format!("plan-{n}")))
            .collect();
        assert_eq!(assign_banners(&plans).len(), plans.len());
    }

    /// An empty kingdom is an ordinary state, not an edge case.
    #[test]
    fn no_plans_is_no_colours() {
        assert!(assign_banners(&[]).is_empty());
    }
}
