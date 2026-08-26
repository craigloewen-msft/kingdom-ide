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
//! # It is now a belt beside a brace
//!
//! `kingdom_browser` disables Chrome's software rasteriser outright
//! (`disable-software-rasterizer`), so a plan's browser has no WebGL context to
//! give the engine even if this decided otherwise. Note that this is *not*
//! achieved by `--disable-gpu`, which was long assumed to do it and does not:
//! measured, a WebGL page under `--disable-gpu` alone still ran a GPU process
//! burning 665% of a core in SwiftShader.
//!
//! The stand-down still earns its keep and is still the primary mechanism. It
//! is what lets the map say something useful instead of failing to acquire a
//! context, and it is decided in the browser, where the one fact that settles
//! it can actually be read.
//!
//! `KINGDOM_BROWSER_WEBGL=on` gives the rasteriser back, for exactly the case
//! `OVERRIDE` below exists to serve: an agent working on this crate needs both
//! -- the query parameter to make the engine boot, and the environment variable
//! to give it something to boot onto.
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
    Draw,
    /// Draw a notice and nothing else. No canvas, no manifest, no Bevy.
    StandDown,
}

impl MapMode {
    /// Whether the engine is to be left unbooted.
    #[must_use]
    pub fn stood_down(self) -> bool {
        matches!(self, MapMode::StandDown)
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
/// Since the software rasteriser is off by default, a plan that wants to *see*
/// the map needs `KINGDOM_BROWSER_WEBGL=on` in the server's environment as well
/// as this parameter. With only this one the engine boots, finds no WebGL
/// context, and the loading card stays up.
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
        Some("on") => MapMode::Draw,
        Some("off") => MapMode::StandDown,
        _ if automated => MapMode::StandDown,
        _ => MapMode::Draw,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ordinary_browser_draws_the_map() {
        assert_eq!(decide(false, None), MapMode::Draw);
    }

    #[test]
    fn an_automated_browser_stands_the_engine_down() {
        assert_eq!(decide(true, None), MapMode::StandDown);
    }

    #[test]
    fn automation_can_ask_for_the_map_anyway() {
        // The case that keeps the map workable by the agents who maintain it.
        assert_eq!(decide(true, Some("on")), MapMode::Draw);
    }

    #[test]
    fn an_ordinary_browser_can_turn_it_off() {
        assert_eq!(decide(false, Some("off")), MapMode::StandDown);
    }

    #[test]
    fn a_value_that_is_neither_is_ignored() {
        // Ignored, not guessed at -- and ignoring it leaves each browser with
        // the default it would have had.
        assert_eq!(decide(false, Some("yes")), MapMode::Draw);
        assert_eq!(decide(true, Some("yes")), MapMode::StandDown);
        assert_eq!(decide(true, Some("")), MapMode::StandDown);
    }
}
