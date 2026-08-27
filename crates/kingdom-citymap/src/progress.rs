//! How much of the manifest has arrived, and how to say so.
//!
//! The map's first wait is a 4 MB fetch, and this is the arithmetic that turns
//! it into a bar and a line of text. Plain numbers and strings: no geometry, no
//! renderer, no framework.
//!
//! # Why this is not in `view`
//!
//! It belongs there -- the view is its only caller. But [`crate::view`] is
//! `hydrate`-only, and `cargo test` builds this crate with **no** features, so
//! anything living there is never compiled by the suite, let alone run by it.
//! Rounding and unit-crossing are exactly the kind of thing that is wrong in a
//! way only a test notices, so the arithmetic sits out here where the suite can
//! reach it, and the view keeps the DOM.
//!
//! It is the second module on both targets, after [`crate::map`], and it is not
//! a seam like that one: nothing on the server reads it. It is here to be
//! tested.

/// What share of the whole wait the fetch is taken to be.
///
/// An **estimate**, in the same spirit as `engine::raise::Step::weight`, and
/// the design assumes so: being wrong makes the bar advance unevenly across the
/// handover and nothing else. It cannot stall either phase, and
/// [`Wait::fraction`] still reaches exactly 1.0 whatever this says.
///
/// Measured on the `kingdom-ide` dev folder (6 towns, 3,028 holdings): a 3.3 s
/// fetch against a 2.5--3 s raise. `tasks/00220` measured 1.9--4.0 s against
/// ~4.6 s on a larger one. The two straddle a half, and this leans slightly
/// towards the raise, because the raise is the half that grows with the
/// kingdom.
pub const FETCH_SHARE: f32 = 0.45;

/// The whole wait the loading card covers, as one number.
///
/// # Why the phases are composed rather than shown in turn
///
/// The card used to ask each phase for its own fraction and draw an
/// indeterminate sweep whenever the one it asked had no answer. That is right
/// for a phase that cannot be measured, and wrong for a phase that has not
/// *started*: measured against a running server, the gap between the manifest
/// arriving and the engine's first slice runs to **1.4 seconds** -- the engine
/// is still waking and the command is sitting in the bridge queue -- and for
/// all of it the King watched a finished 4 MB download reported as a bar that
/// knew nothing.
///
/// Composing them removes the question. There is one scale over the whole wait,
/// the fetch fills the first [`FETCH_SHARE`] of it, the raise fills the rest,
/// and a phase that has not begun leaves the bar where the last one left it.
/// Nothing is invented: every number here is either measured work or a boundary
/// between two measured phases.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Wait {
    /// How much of the manifest has arrived, while it is arriving.
    pub transfer: Transfer,
    /// Whether the manifest is in hand. The fetch's share is complete from this
    /// moment, whatever the byte counts did or did not manage to say.
    pub arrived: bool,
    /// How far through raising the world, once the engine has begun.
    pub raising: Option<f32>,
    /// Whether the world is standing, which pins the bar full.
    pub built: bool,
}

impl Wait {
    /// How far through the whole wait, or `None` for a bar with no fraction.
    ///
    /// The one remaining `None` is the case [`Transfer::fraction`] was written
    /// for: a server that declared no length, or a count that has outrun what it
    /// declared. Then the first segment genuinely cannot be measured and the
    /// card sweeps -- until the manifest lands, from which point there is a
    /// boundary and then real work to report again.
    ///
    /// It is monotonic by construction, and that is the point: each phase is
    /// confined to its own segment of the scale, so no handover can step the bar
    /// backwards.
    pub fn fraction(&self) -> Option<f32> {
        if self.built {
            return Some(1.0);
        }
        if let Some(raising) = self.raising {
            return Some(FETCH_SHARE + raising.clamp(0.0, 1.0) * (1.0 - FETCH_SHARE));
        }
        if self.arrived {
            // The handover: the fetch is done and the raise has not started.
            // Held at the boundary rather than swept, because "the next thing
            // has not begun" is not the same as "nothing is known".
            return Some(FETCH_SHARE);
        }
        Some(self.transfer.fraction()? * FETCH_SHARE)
    }
}

/// How much of the manifest has been read, and how much there is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Transfer {
    /// Bytes read so far.
    pub read: u64,
    /// Bytes the server said it would send, if it said.
    pub total: Option<u64>,
}

impl Transfer {
    /// How far along, from 0.0 to 1.0, or `None` when that cannot be known.
    ///
    /// `None` is a bar with no fraction on it rather than a bar at zero, and
    /// there are two ways to reach it. A server that sent no `content-length`
    /// never said how much there was. And a count that has passed the declared
    /// total means the two numbers are measuring different things -- which is
    /// what a compression layer in front of this route would do, since
    /// `content-length` would then be the compressed size while these bytes
    /// are decompressed. A bar reading 640% is worse than one that admits it
    /// does not know.
    pub fn fraction(&self) -> Option<f32> {
        let total = self.total.filter(|total| *total > 0)?;
        if self.read > total {
            return None;
        }
        Some((self.read as f32 / total as f32).clamp(0.0, 1.0))
    }

    /// What the card says under the phase, such as `2.4 MB of 4.2 MB`.
    ///
    /// Falls back to what is known: the bytes alone before a total is in hand,
    /// and a plain sentence before the first byte, because "0 B" reads as
    /// nothing happening at the exact moment the King is most likely to be
    /// wondering whether anything is.
    pub fn detail(&self) -> String {
        match (self.read, self.total) {
            (0, _) => "Reading every city in the kingdom".to_owned(),
            (read, Some(total)) if read <= total => {
                format!("{} of {}", bytes(read), bytes(total))
            }
            (read, _) => format!("{} so far", bytes(read)),
        }
    }
}

/// A byte count as the King would read it aloud.
///
/// One decimal place from a megabyte up, none below: the tenths of a megabyte
/// are what make the number visibly move on a wait this long, while tenths of a
/// kilobyte are noise going past too fast to read.
pub fn bytes(count: u64) -> String {
    const KB: f64 = 1_024.0;
    const MB: f64 = KB * 1_024.0;
    const GB: f64 = MB * 1_024.0;

    let count = count as f64;
    if count >= GB {
        format!("{:.1} GB", count / GB)
    } else if count >= MB {
        format!("{:.1} MB", count / MB)
    } else if count >= KB {
        format!("{:.0} KB", count / KB)
    } else {
        format!("{count:.0} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_transfer_reads_as_a_fraction_of_what_was_promised() {
        let half = Transfer {
            read: 2_175_920,
            total: Some(4_351_840),
        };
        assert_eq!(half.fraction(), Some(0.5));
        assert_eq!(half.detail(), "2.1 MB of 4.2 MB");
    }

    /// The bar must not be able to leave its track, whatever the two numbers
    /// turn out to be measuring. See [`Transfer::fraction`].
    #[test]
    fn a_count_that_outruns_the_total_admits_it_does_not_know() {
        let overrun = Transfer {
            read: 4_351_840,
            total: Some(693_981),
        };
        assert_eq!(overrun.fraction(), None);
        // And it says something true rather than "4.2 MB of 678 KB".
        assert_eq!(overrun.detail(), "4.2 MB so far");
    }

    #[test]
    fn a_server_that_promised_nothing_gets_an_indeterminate_bar() {
        let unknown = Transfer {
            read: 12_000,
            total: None,
        };
        assert_eq!(unknown.fraction(), None);
        assert_eq!(unknown.detail(), "12 KB so far");

        // A zero-length body is the same absence of information, not a
        // division by zero.
        let empty = Transfer {
            read: 0,
            total: Some(0),
        };
        assert_eq!(empty.fraction(), None);
    }

    #[test]
    fn nothing_read_yet_says_what_is_happening_rather_than_zero() {
        let started = Transfer {
            read: 0,
            total: Some(4_351_840),
        };
        assert_eq!(started.fraction(), Some(0.0));
        assert!(
            !started.detail().contains('0'),
            "0 B reads as nothing happening: {}",
            started.detail()
        );
    }

    #[test]
    fn byte_counts_are_scaled_to_something_readable() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(1_024), "1 KB");
        assert_eq!(bytes(1_048_576), "1.0 MB");
        assert_eq!(bytes(4_351_840), "4.2 MB");
        assert_eq!(bytes(2_147_483_648), "2.0 GB");
    }

    /// The whole point of composing the two phases: the moment between them is
    /// a held bar, not a bar that has forgotten what it knew.
    ///
    /// Measured against a running server this gap lasts up to 1.4 seconds, so
    /// it is a state the King looks at rather than one frame of arithmetic.
    #[test]
    fn the_gap_between_the_phases_holds_the_bar_where_the_fetch_left_it() {
        let full = Transfer {
            read: 4_351_840,
            total: Some(4_351_840),
        };
        let handover = Wait {
            transfer: full,
            arrived: true,
            raising: None,
            built: false,
        };
        assert_eq!(handover.fraction(), Some(FETCH_SHARE));

        // And the first slice of the raise picks up from exactly there rather
        // than starting again from nothing.
        let first_slice = Wait {
            raising: Some(0.0),
            ..handover
        };
        assert_eq!(first_slice.fraction(), Some(FETCH_SHARE));
    }

    /// The bar may never go backwards, whatever order the phases report in.
    ///
    /// This is what the segments buy. Each phase is confined to its own stretch
    /// of the scale, so no handover can undo work the King has already watched
    /// complete.
    #[test]
    fn the_bar_only_ever_moves_forwards() {
        let total = Some(4_351_840);
        let mut readings = Vec::new();

        for read in [0, 1_000_000, 2_175_920, 4_351_840] {
            readings.push(Wait {
                transfer: Transfer { read, total },
                ..Wait::default()
            });
        }
        readings.push(Wait {
            transfer: Transfer {
                read: 4_351_840,
                total,
            },
            arrived: true,
            ..Wait::default()
        });
        for raising in [0.0, 0.1, 0.62, 0.99] {
            readings.push(Wait {
                arrived: true,
                raising: Some(raising),
                ..Wait::default()
            });
        }
        readings.push(Wait {
            arrived: true,
            built: true,
            ..Wait::default()
        });

        let mut last = 0.0;
        for reading in readings {
            let now = reading
                .fraction()
                .expect("every reading here is measurable");
            assert!(
                now >= last,
                "the bar went backwards, from {last} to {now}: {reading:?}"
            );
            last = now;
        }
        assert_eq!(last, 1.0);
    }

    /// The fetch is the first stretch of the scale rather than the whole of it,
    /// so a completed download does not read as a completed map.
    #[test]
    fn the_fetch_fills_only_its_own_share() {
        let half_way = Wait {
            transfer: Transfer {
                read: 2_175_920,
                total: Some(4_351_840),
            },
            ..Wait::default()
        };
        assert_eq!(half_way.fraction(), Some(FETCH_SHARE / 2.0));

        // And the raise fills the rest, from the boundary to the end.
        let mid_raise = Wait {
            arrived: true,
            raising: Some(0.5),
            ..Wait::default()
        };
        assert_eq!(
            mid_raise.fraction(),
            Some(FETCH_SHARE + (1.0 - FETCH_SHARE) / 2.0)
        );
    }

    /// A server that said nothing about length still gets the sweep, because
    /// that is the one case where the arithmetic genuinely has no answer.
    ///
    /// It is not the same as the gap above, and conflating the two is what put
    /// an indeterminate bar over a finished download.
    #[test]
    fn an_unmeasurable_fetch_still_declines_to_answer() {
        let unknown = Wait {
            transfer: Transfer {
                read: 12_000,
                total: None,
            },
            ..Wait::default()
        };
        assert_eq!(unknown.fraction(), None);

        // But only until the manifest is in hand: from there the wait has a
        // boundary to report and a raise to measure, whatever the headers said.
        let arrived = Wait {
            arrived: true,
            ..unknown
        };
        assert_eq!(arrived.fraction(), Some(FETCH_SHARE));
    }

    /// Finishing is a state of the bar, not the absence of one.
    ///
    /// `raising` is cleared the moment the world stands and the card then
    /// spends 320 ms fading, so without this the King's last sight of the bar
    /// would be it emptying at the moment the work succeeded.
    #[test]
    fn a_world_that_stands_pins_the_bar_full() {
        let done = Wait {
            arrived: true,
            raising: None,
            built: true,
            ..Wait::default()
        };
        assert_eq!(done.fraction(), Some(1.0));

        // Even if the engine is somehow still reporting a part-built world.
        let disagreeing = Wait {
            raising: Some(0.3),
            ..done
        };
        assert_eq!(disagreeing.fraction(), Some(1.0));
    }

    /// Nothing has happened yet, which is a bar at zero rather than a bar that
    /// cannot say -- the card is up, and the fetch is under way.
    #[test]
    fn a_wait_that_has_just_begun_reads_as_empty_rather_than_unknown() {
        let starting = Wait {
            transfer: Transfer {
                read: 0,
                total: Some(4_351_840),
            },
            ..Wait::default()
        };
        assert_eq!(starting.fraction(), Some(0.0));
    }
}
