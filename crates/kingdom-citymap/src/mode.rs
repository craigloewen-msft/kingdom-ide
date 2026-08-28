//! Whether the map draws itself at all.
//!
//! The engine is a renderer, and a renderer in a browser nobody is watching is
//! pure cost. Kingdom's own `browser_*` tools drive a headless Chrome, and a
//! map opened by automation is therefore stood down: the notice is drawn and
//! the engine never boots.
//!
//! # How much this saves, measured
//!
//! A great deal, and it is worth knowing the figure before anyone is tempted to
//! simplify this away. The chamber in a headless Chrome that reports
//! `navigator.webdriver` costs about 5% of a core; the same chamber in one that
//! does not -- so the engine boots -- cost 790%. Nearly eight cores, on a
//! machine several agents are sharing.
//!
//! Most of that is now recoverable rather than merely avoidable -- see the
//! table below -- but avoiding it entirely is still free, and still right for
//! the great majority of plans, which never look at the map at all.
//!
//! # It is now a belt beside a brace
//!
//! `kingdom_browser` gives a plan's browser WebGL by default, and bounds what
//! it can cost two ways: the engine holds itself to `engine::AUTOMATED_WAKE`
//! when this decides [`MapMode::Draw`] with `capped`, and the browser is
//! confined to a few CPUs (`session::CPUS_VAR`).
//!
//! Both are needed, and the reason is worth knowing before anyone simplifies
//! either away. Measured on this map, world standing, nothing happening:
//!
//! | | Cost |
//! |---|---|
//! | uncapped, unconfined | 9.50 cores |
//! | one frame a second, unconfined | 4.09 cores |
//! | capped and confined to four CPUs | 2.03 cores |
//!
//! The middle row is the surprising one and it is why pacing alone was not the
//! answer. Chrome has no GPU here, so it rasterises in SwiftShader, whose
//! thread pool sizes itself from the machine and spends most of what it spends
//! whether or not a frame was asked for. Slowing the engine cuts the frames;
//! only the CPU ceiling cuts the floor underneath them.
//!
//! Note also what `--disable-gpu` does *not* do: it does not stop any of this.
//! It turns off hardware acceleration, and on a machine with no usable GPU --
//! which is every machine a headless browser runs on -- that changes nothing.
//!
//! The stand-down remains the primary mechanism and the default. It is what
//! lets the map say something useful instead of costing anything at all, and
//! it is decided in the browser, where the one fact that settles it can
//! actually be read.
//!
//! `?map=on` is now enough on its own. The engine boots, the rasteriser is
//! there to boot onto, and the pace and the ceiling keep the bill in sight --
//! so an agent working on this crate can simply open the map and look at it.
//!
//! # Why the decision is made here and not on the server
//!
//! One server process serves the King's own browser and a plan's headless one,
//! at the same URL, at the same moment. An environment variable or a server
//! flag would answer for both at once, and answering for both is exactly the
//! thing that cannot be done -- only the client knows which of the two it is.
//!
//! So the two facts are read in the browser (`view.rs`, where they cannot be
//! tested because there is no DOM under `cargo test`) and the decision is
//! taken here, in a function that knows nothing about a DOM and can
//! therefore be tested by the native `cargo test -p kingdom-citymap` that never
//! touches wasm.

/// What the map does with itself on this page load.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapMode {
    /// Boot the engine, fetch the manifest, draw the world.
    Draw {
        /// Whether to hold the engine to a frame ceiling while it does.
        ///
        /// Carried on the variant rather than answered separately because the
        /// two facts have one source: a browser is told to draw *and* held to a
        /// ceiling for the same reason, that it is automation which asked.
        capped: bool,
    },
    /// Draw a notice and nothing else. No canvas, no manifest, no Bevy.
    StandDown,
}

impl MapMode {
    /// Whether the engine is to be left unbooted.
    #[must_use]
    pub fn stood_down(self) -> bool {
        matches!(self, MapMode::StandDown)
    }

    /// Whether an engine that does boot must be held to a frame ceiling.
    ///
    /// True only for [`MapMode::Draw`] reached *against* the automated default
    /// -- which is to say `?map=on` in a driven browser. That page is drawn for
    /// something that will read it and move on, so the frames between reads are
    /// pure cost, and on such a browser they are software-rasterised ones.
    ///
    /// Measured, the difference this decides is 9.50 cores against 0.00. See
    /// `engine::AUTOMATED_WAKE`, which is the ceiling itself.
    #[must_use]
    pub fn capped(self) -> bool {
        matches!(self, MapMode::Draw { capped: true })
    }
}

/// The query parameter that overrides the default in either direction.
///
/// `?map=on` is the important one. Without it this change would make the map
/// unverifiable by the very agents most likely to be asked to change it -- a
/// plan working on `kingdom-citymap` needs to be able to look at what it drew.
/// `?map=off` is its cheap mirror, for anyone working on Kingdom in an ordinary
/// browser who would rather their fans were quiet.
///
/// It is now sufficient on its own: a plan's browser has WebGL by default, so
/// this parameter both boots the engine and gives it something to boot onto.
/// The engine paces itself and the browser is confined, so taking the
/// invitation costs about two cores rather than nine and a half.
pub const OVERRIDE: &str = "map";

/// Decides from the two facts the browser can report.
///
/// `automated` is `navigator.webdriver`, and `forced` is the `map` query
/// parameter if one was given. An override wins over the default in both
/// directions; a value that is neither `on` nor `off` is ignored rather than
/// guessed at, because a typo'd flag silently meaning its opposite is worse
/// than a typo'd flag meaning nothing.
#[must_use]
pub fn decide(automated: bool, forced: Option<&str>) -> MapMode {
    match forced {
        // The one case that is drawn *and* paced: automation took the
        // invitation to look at the map. It gets a real engine, a real world
        // and real picking -- at a frame rate nobody is watching for.
        Some("on") => MapMode::Draw { capped: automated },
        Some("off") => MapMode::StandDown,
        _ if automated => MapMode::StandDown,
        _ => MapMode::Draw { capped: false },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ordinary_browser_draws_the_map() {
        assert_eq!(decide(false, None), MapMode::Draw { capped: false });
    }

    #[test]
    fn an_automated_browser_stands_the_engine_down() {
        assert_eq!(decide(true, None), MapMode::StandDown);
    }

    #[test]
    fn automation_can_ask_for_the_map_anyway() {
        // The case that keeps the map workable by the agents who maintain it.
        assert_eq!(decide(true, Some("on")), MapMode::Draw { capped: true });
    }

    #[test]
    fn the_kings_own_map_is_never_paced() {
        // The other half, and the one that must not regress: a person at a
        // real browser flies through this map, and capping it would be felt.
        assert!(!decide(false, None).capped());
        assert!(!decide(false, Some("on")).capped());
    }

    #[test]
    fn a_stood_down_map_is_not_described_as_paced() {
        // `capped` answers for an engine that boots. One that does not is not
        // "capped", it is absent -- and a caller reading it the other way would
        // be asking the wrong question entirely.
        assert!(!decide(true, None).capped());
        assert!(!decide(false, Some("off")).capped());
    }

    #[test]
    fn an_ordinary_browser_can_turn_it_off() {
        assert_eq!(decide(false, Some("off")), MapMode::StandDown);
    }

    #[test]
    fn a_value_that_is_neither_is_ignored() {
        // Ignored, not guessed at -- and ignoring it leaves each browser with
        // the default it would have had.
        assert_eq!(decide(false, Some("yes")), MapMode::Draw { capped: false });
        assert_eq!(decide(true, Some("yes")), MapMode::StandDown);
        assert_eq!(decide(true, Some("")), MapMode::StandDown);
    }
}
