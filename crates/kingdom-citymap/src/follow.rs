//! When the rail's map is allowed to move its camera, and where to.
//!
//! # Why this is a module and not two effects
//!
//! It was two effects in [`crate::view`] -- one framing the town a chamber is
//! about, one pointing at the building of the file open in front of the King --
//! each with its own memory of what it had last done, and both able to fire on
//! a single wake. That arrangement had no way to express the rule the King
//! actually wants, which is a rule about *both* of them at once: the camera
//! moves when he opens a file, when the chamber becomes about a different city,
//! and when the map changes home. Nothing else.
//!
//! Two effects cannot state that, because neither can see what the other is
//! about to do. One function can, and the reason it is a *pure* function is
//! [`crate::engine::input`]'s: `view.rs` is `hydrate`-only and there is no DOM
//! under `cargo test`, so a decision left in an effect is a decision nothing
//! can pin. Here it is arithmetic over six plain values, and the tests below
//! are the evidence.
//!
//! # The fault this was written for
//!
//! The map swept and re-zoomed on its own while the King was reading, roughly
//! once per round of any agent working in that project. The cause was not here
//! -- the chamber was re-announcing the same city on every socket push, and
//! `reactive_graph`'s `set` notifies whether or not the value moved, so both
//! effects re-ran and re-sent their commands. That is fixed at the source, in
//! `kingdom_app::components::conversation`.
//!
//! This module is what makes it *stay* fixed. A guard at one call site is one
//! line away from being lost; a rule that answers [`Step::Stay`] unless
//! something it names has genuinely changed cannot be undone by accident,
//! because the tests say what the answers are.

/// What the map last did to its own camera.
///
/// Two fields rather than one, because "which town the camera was put on" and
/// "which building it is currently down among" are different questions and are
/// asked separately. [`Followed::inspected`] is `None` whenever the camera is
/// framing a whole town, which is what makes the first file opened there an
/// arrival rather than a hop -- see [`Step::Inspect`].
///
/// The inspected half carries the **path as well as the city**, and that is the
/// reported fault rather than a detail. Remembering only the city cannot tell
/// "the King opened another file here" from "something woke this and nothing
/// has changed" -- and the second is what an agent writing a file, a status
/// poll and a pan all are. A rule that cannot tell them apart re-aims the
/// camera at the building it is already pointed at, which is the twitching this
/// module was written to stop. The first draft of this module got that wrong
/// and the tests below caught it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Followed {
    /// The city whose town the camera was last framed on.
    pub framed: Option<String>,
    /// The city and path an [`Step::Inspect`] last pointed at, if the camera is
    /// on a building rather than on a town.
    pub inspected: Option<(String, String)>,
}

impl Followed {
    /// Forgets everything, so the next decision treats the camera as somewhere
    /// this rule did not put it.
    ///
    /// Called when the King takes the map by hand and when it changes home:
    /// both mean the camera is about to be somewhere [`decide`] cannot predict,
    /// and a memory of a building it is no longer at would turn the next
    /// arrival into a glide from nowhere.
    pub fn forget(&mut self) {
        self.framed = None;
        self.inspected = None;
    }

    /// Records a town having been framed.
    ///
    /// Clears [`Self::inspected`] deliberately: the camera is now above the
    /// whole place rather than among its buildings.
    pub fn frame(&mut self, city: &str) {
        self.framed = Some(city.to_owned());
        self.inspected = None;
    }

    /// Records the camera having been pointed at one building.
    pub fn inspect(&mut self, city: &str, path: &str) {
        self.framed = Some(city.to_owned());
        self.inspected = Some((city.to_owned(), path.to_owned()));
    }

    /// Whether the camera is already pointed at this very building.
    fn already_at(&self, city: &str, path: &str) -> bool {
        self.inspected
            .as_ref()
            .is_some_and(|(at, file)| at == city && file == path)
    }

    /// Whether the camera is among the buildings of this city, wherever in it.
    fn among(&self, city: &str) -> bool {
        self.inspected.as_ref().is_some_and(|(at, _)| at == city)
    }
}

/// What the map should do to its camera now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Nothing. The common answer, and the whole point of this module.
    Stay,
    /// Frame the town of the city the chamber is about.
    ///
    /// Only for a city the camera has not been put on, so arriving in a chamber
    /// still shows the King the place his plan is about rather than leaving the
    /// map parked on the last project he looked at.
    Frame,
    /// Point at the building of the file he has open.
    Inspect {
        /// Whether to travel there rather than cutting.
        ///
        /// A hop between two files of one city is short enough for the eye to
        /// follow, and the glide is what tells him which way the map went.
        /// Arriving in a city is not a journey worth animating: the whole frame
        /// changes, so a tween across it is a smear. See
        /// [`crate::engine::bridge::ViewerCommand::Inspect`].
        glide: bool,
    },
}

/// The rule, in one place.
///
/// `city` is the city the chamber is about and `file` the path open in its
/// panel, both as the interface currently has them. `in_rail` and `built` are
/// the two conditions under which moving the camera means anything at all, and
/// `manual` is whether the King has taken it.
///
/// Returns [`Step::Stay`] for every wake that does not name a *new* place to
/// be, which is what a poll landing, an agent writing a file, or a pan of the
/// map all are.
pub fn decide(
    last: &Followed,
    in_rail: bool,
    built: bool,
    manual: bool,
    city: Option<&str>,
    file: Option<&str>,
) -> Step {
    // Not in the rail, or nothing standing to point at: on the King's own map
    // the camera has always been his, and there is no world to aim at before
    // one has been raised.
    if !in_rail || !built || manual {
        return Step::Stay;
    }
    let Some(city) = city else {
        return Step::Stay;
    };

    // A city the camera has not been put on. This is the one automatic move
    // that survives: without it, walking into a chamber about another project
    // would leave the map showing the project he just left.
    if last.framed.as_deref() != Some(city) {
        return Step::Frame;
    }

    // The town is already framed, so the only thing left that may move the
    // camera is a file. No file open means the King closed the panel -- and
    // that leaves the camera exactly where it is. Pulling back out to the town
    // is motion he did not ask for, and the building it is looking at is still
    // a building of the city he is in.
    let Some(path) = file else {
        return Step::Stay;
    };

    // The camera is already on this very building. This is the answer for
    // almost every wake -- an agent writing a file, a status poll landing, a
    // pan -- and giving it is the whole point of the module.
    if last.already_at(city, path) {
        return Step::Stay;
    }

    Step::Inspect {
        // Already among this city's buildings, so this is a hop the eye can
        // follow. Coming from the town-wide frame is an arrival instead.
        glide: last.among(city),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn among(city: &str, path: &str) -> Followed {
        let mut last = Followed::default();
        last.inspect(city, path);
        last
    }

    fn above(city: &str) -> Followed {
        let mut last = Followed::default();
        last.frame(city);
        last
    }

    /// The reported fault, as a test.
    ///
    /// An agent writing a file wakes everything that reads the kingdom, and
    /// nothing about where the King is looking has changed. The camera must not
    /// move -- this is the whole complaint.
    #[test]
    fn nothing_new_moves_nothing() {
        let last = among("forge", "src/lib.rs");
        assert_eq!(
            decide(&last, true, true, false, Some("forge"), Some("src/lib.rs")),
            Step::Stay,
            "the same city and the same file is not news"
        );
    }

    /// Opening a file is the one thing that earns a move.
    #[test]
    fn opening_a_file_points_at_it() {
        let last = above("forge");
        assert_eq!(
            decide(&last, true, true, false, Some("forge"), Some("src/lib.rs")),
            Step::Inspect { glide: false },
            "arriving from the town-wide frame is not a journey to animate"
        );
    }

    /// And moving between two files of one city is a hop the eye can follow.
    #[test]
    fn moving_between_files_glides() {
        let last = among("forge", "src/lib.rs");
        assert_eq!(
            decide(&last, true, true, false, Some("forge"), Some("src/main.rs")),
            Step::Inspect { glide: true }
        );
    }

    /// The King's ruling: a closed panel leaves the camera where it is.
    ///
    /// This reverses what the map used to do, which was to pull back out to the
    /// whole town. That was motion nobody asked for, and the building it was
    /// looking at is still a building of the city he is in.
    #[test]
    fn closing_the_panel_leaves_the_camera_alone() {
        let last = among("forge", "src/lib.rs");
        assert_eq!(
            decide(&last, true, true, false, Some("forge"), None),
            Step::Stay
        );
    }

    /// The other automatic move that survives, and why it has to.
    ///
    /// Without it a chamber about another project opens onto a map still
    /// showing the last one -- a pane confidently displaying the wrong place.
    #[test]
    fn a_new_city_is_framed_once() {
        assert_eq!(
            decide(
                &among("forge", "src/lib.rs"),
                true,
                true,
                false,
                Some("mill"),
                Some("src/lib.rs")
            ),
            Step::Frame
        );
        // And having framed it, the file is what it narrows to next.
        assert_eq!(
            decide(
                &above("mill"),
                true,
                true,
                false,
                Some("mill"),
                Some("src/lib.rs")
            ),
            Step::Inspect { glide: false },
            "the first file of a new city is an arrival, not a hop"
        );
    }

    /// While the King holds the map, nothing follows anything.
    #[test]
    fn a_map_taken_by_hand_is_never_moved() {
        assert_eq!(
            decide(
                &above("forge"),
                true,
                true,
                true,
                Some("mill"),
                Some("src/lib.rs")
            ),
            Step::Stay
        );
    }

    /// And on his own map the camera has always been his.
    #[test]
    fn the_kings_own_map_is_not_steered() {
        assert_eq!(
            decide(
                &Followed::default(),
                false,
                true,
                false,
                Some("forge"),
                Some("src/lib.rs")
            ),
            Step::Stay
        );
    }

    /// Nothing is pointed at before there is a world to point at. A world's
    /// raise ends with its own `fit()`, which would overwrite anything sent
    /// while it was going up.
    #[test]
    fn nothing_is_aimed_at_a_world_still_going_up() {
        assert_eq!(
            decide(
                &Followed::default(),
                true,
                false,
                false,
                Some("forge"),
                Some("src/lib.rs")
            ),
            Step::Stay
        );
    }

    /// Forgetting turns the next move back into an arrival.
    ///
    /// What the free-look chip and a change of home both rely on: the camera is
    /// somewhere this rule did not put it, so the memory of a building it is no
    /// longer at would produce a glide from nowhere.
    #[test]
    fn forgetting_makes_the_next_move_an_arrival() {
        let mut last = among("forge", "src/lib.rs");
        last.forget();
        assert_eq!(
            decide(&last, true, true, false, Some("forge"), Some("src/lib.rs")),
            Step::Frame
        );
    }

    /// Framing a town forgets the building, so returning to a file there is an
    /// arrival rather than a hop from wherever the camera last stood.
    #[test]
    fn framing_a_town_leaves_the_buildings_behind() {
        let mut last = among("forge", "src/lib.rs");
        last.frame("forge");
        assert_eq!(last.inspected, None);
    }
}
