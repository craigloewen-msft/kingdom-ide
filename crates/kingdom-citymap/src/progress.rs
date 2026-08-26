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
}
