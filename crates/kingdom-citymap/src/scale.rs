//! One curve, shared by everything on the map that is sized from a count.
//!
//! # The rule
//!
//! **Twice the lines, twice the mark.** A holding's footprint already obeys it
//! -- `build::layout::weight` is linear in a file's lines -- and this is that
//! same rule made available to the two marks that did not: the column a plan
//! raises for the lines it added, and the driveway a much-imported file earns.
//!
//! # Why it is not simply linear
//!
//! Because a footprint and a column are bounded differently, and that difference
//! is the whole of the design here.
//!
//! A lot is a *share of its folder's ground*, so a file with a hundred times the
//! content can be given a hundred times the land and the ward simply grows. A
//! column has no such freedom: it is one axis with a hard ceiling --
//! `engine::works::COLUMN_REACH`, held under the 60 world units the camera's fit
//! reserves for a roofline -- so *something* has to give at the top or the
//! tallest column is one the King can zoom until it is cut off.
//!
//! So the range is spent deliberately: **strictly proportional up to a knee, and
//! a saturating tail above it**. Under the knee, doubling the count doubles the
//! mark exactly. Over it, the mark keeps growing forever without ever reaching
//! the ceiling -- which is what keeps a 935-line change and a 4,000-line one
//! different marks.
//!
//! The knee is not a matter of taste. It is a claim about the distribution of
//! the thing being drawn, and each caller fits it to its own measured data --
//! see `engine::works::LINEAR_CHURN` and `build::streets::DRIVE_LINEAR_ARRIVALS`.
//!
//! # Why the tail meets the line at its own slope
//!
//! [`linear_then_tail`] derives the tail's own constant from the knee rather
//! than taking it as a parameter, so the two pieces join at *matching slope*.
//! A tail chosen independently makes a visible corner: the mark climbs steadily
//! and then abruptly slows, and the reader sees a threshold in the data where
//! there is none. Deriving it means there is one shape, not two glued together.
//!
//! # Why this module is on every target
//!
//! `build` is `ssr` and `engine` is `hydrate`, and this is needed by both. It is
//! also the reason `progress` is unconditional: `cargo test` builds this crate
//! with no features at all, so arithmetic that lives here is arithmetic the bare
//! suite pins, on a machine with no browser and no filesystem to scan.

/// How much of its range a count has earned: `0.0..1.0`, never reaching 1.0.
///
/// Proportional up to `knee`, where it returns exactly `knee_share`; above it, a
/// saturating tail over the remaining `1.0 - knee_share`.
///
/// # Reading the two parameters
///
/// - `knee` is **where proportionality stops**, in the caller's own units. Put
///   it at the top of the range the data actually occupies -- past the p95 of a
///   real distribution, not at its median -- because everything below it is
///   drawn exactly to scale and everything above it is compressed.
/// - `knee_share` is **how much of the mark the linear part spends**. The
///   remainder is what the entire tail has left to work with, so a larger share
///   buys fidelity in the common range at the cost of resolution among the
///   rewrites.
///
/// # Why NaN is handled rather than assumed away
///
/// The counts arrive as `f32` over a wire. `f32::clamp` propagates NaN rather
/// than trapping it, so an unguarded version yields a NaN height, which reaches
/// Bevy as a degenerate mesh. A predecessor of this function was found doing
/// exactly that by a test rather than in a browser, and the guard is kept.
///
/// `f32::INFINITY` and NaN both come out as 0.0, because the guard returns
/// before any arithmetic is reached. Neither is a count, so neither gets a mark;
/// a change so large it saturates in *finite* arithmetic still does, which is
/// the case that matters.
pub fn linear_then_tail(value: f32, knee: f32, knee_share: f32) -> f32 {
    if !value.is_finite() || value <= 0.0 {
        return 0.0;
    }
    // A knee at or below zero would make every count the largest there is, and
    // a share outside `0..1` has no meaning as a share. Both are programmer
    // error rather than data, but this is drawn from a wire and a degenerate
    // mesh is a worse answer than a dull one.
    if !knee.is_finite() || knee <= 0.0 {
        return knee_share.clamp(0.0, 1.0);
    }
    let share = knee_share.clamp(0.0, 1.0);
    if value <= knee {
        return share * value / knee;
    }
    // The tail's constant, derived so the curve's slope is continuous at the
    // knee: the line climbs `share / knee` per unit, and a saturating ratio
    // `e / (e + tail)` scaled by `1 - share` climbs `(1 - share) / tail` at
    // `e = 0`. Setting those equal gives this.
    let remaining = 1.0 - share;
    if remaining <= 0.0 {
        return share;
    }
    let tail = remaining * knee / share.max(f32::MIN_POSITIVE);
    let past = value - knee;
    (share + remaining * (past / (past + tail))).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The King's own rule**, which is the whole reason this module exists:
    /// under the knee, twice the count is exactly twice the mark.
    #[test]
    fn doubling_below_the_knee_doubles_the_mark() {
        for (small, large) in [(1.0, 2.0), (8.0, 16.0), (50.0, 100.0), (75.0, 150.0)] {
            let ratio = linear_then_tail(large, 300.0, 0.75) / linear_then_tail(small, 300.0, 0.75);
            assert!(
                (ratio - 2.0).abs() < 1e-5,
                "{large} against {small} drew {ratio}x, which is not proportional"
            );
        }
    }

    /// Proportionality is not only about doubling: any ratio under the knee is
    /// the ratio of the counts themselves.
    #[test]
    fn any_ratio_below_the_knee_is_the_counts_own_ratio() {
        for (small, large) in [(6.0, 27.0), (27.0, 115.0), (10.0, 250.0)] {
            let drawn = linear_then_tail(large, 300.0, 0.75) / linear_then_tail(small, 300.0, 0.75);
            let want = large / small;
            assert!(
                (drawn / want - 1.0).abs() < 1e-4,
                "{large}/{small} is {want}x of work and drew {drawn}x"
            );
        }
    }

    /// The knee is exactly where the linear part stops, and it is worth its
    /// share to the last decimal -- the two pieces meet rather than overlap.
    #[test]
    fn the_knee_is_worth_exactly_its_share() {
        assert!((linear_then_tail(300.0, 300.0, 0.75) - 0.75).abs() < 1e-6);
        assert!((linear_then_tail(16.0, 16.0, 0.8) - 0.8).abs() < 1e-6);
    }

    /// **No plateau, ever.** This is the fault a previous curve had -- a clamp
    /// at 600 lines drew every larger change identically -- and the tail exists
    /// precisely so that it cannot come back.
    #[test]
    fn the_tail_never_stops_growing() {
        let mag = |v: f32| linear_then_tail(v, 300.0, 0.75);
        let mut last = mag(300.0);
        for value in [301.0, 400.0, 600.0, 935.0, 2_137.0, 4_000.0, 20_000.0, 1e6] {
            let now = mag(value);
            assert!(now > last, "{value} did not grow past the value before it");
            assert!(now < 1.0, "{value} reached the ceiling, which is a plateau");
            last = now;
        }
    }

    /// The join is smooth: approaching the knee from either side, the curve
    /// climbs at the same rate. A mismatch here is a visible corner on screen.
    #[test]
    fn the_tail_meets_the_line_at_the_same_slope() {
        let (knee, share) = (300.0_f32, 0.75_f32);
        let step = 0.01_f32;
        let below = (linear_then_tail(knee, knee, share)
            - linear_then_tail(knee - step, knee, share))
            / step;
        let above = (linear_then_tail(knee + step, knee, share)
            - linear_then_tail(knee, knee, share))
            / step;
        assert!(
            (below / above - 1.0).abs() < 0.01,
            "the curve climbs {below} below the knee and {above} above it, which is a corner"
        );
    }

    /// Never outside `0.0..1.0`, whatever arrives -- including the values that
    /// cross a wire as `f32` and are not counts at all.
    #[test]
    fn nothing_leaves_the_range() {
        for value in [
            -1.0,
            0.0,
            1.0,
            1e9,
            f32::MAX,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::NAN,
        ] {
            let drawn = linear_then_tail(value, 300.0, 0.75);
            assert!(
                (0.0..=1.0).contains(&drawn),
                "{value} drew {drawn}, which is outside the range"
            );
            assert!(
                drawn.is_finite(),
                "{value} drew a mark that is not a number"
            );
        }
        assert_eq!(linear_then_tail(f32::NAN, 300.0, 0.75), 0.0);
        assert_eq!(linear_then_tail(f32::INFINITY, 300.0, 0.75), 0.0);
        assert_eq!(linear_then_tail(-5.0, 300.0, 0.75), 0.0);
    }

    /// A degenerate knee is programmer error, not data, but it arrives on the
    /// same wire -- so it yields a dull mark rather than a degenerate mesh.
    #[test]
    fn a_nonsense_knee_still_draws_something_sane() {
        for knee in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            let drawn = linear_then_tail(100.0, knee, 0.75);
            assert!(drawn.is_finite() && (0.0..=1.0).contains(&drawn));
        }
        // A share that spends the whole range leaves the tail nothing, which is
        // a clamp -- allowed, but it must not divide by zero or overshoot.
        let all = linear_then_tail(10_000.0, 300.0, 1.0);
        assert!(all.is_finite() && all <= 1.0);
        // And a share of nothing cannot divide by zero either.
        let none = linear_then_tail(10_000.0, 300.0, 0.0);
        assert!(none.is_finite() && (0.0..=1.0).contains(&none));
    }

    /// Monotonic everywhere, not merely within each piece: a bigger count is
    /// never a smaller mark, including across the join.
    #[test]
    fn a_bigger_count_is_never_a_smaller_mark() {
        let mut last = 0.0;
        let mut value = 0.0;
        while value < 5_000.0 {
            let now = linear_then_tail(value, 300.0, 0.75);
            assert!(now >= last, "{value} drew less than the count before it");
            last = now;
            value += 0.5;
        }
    }
}
