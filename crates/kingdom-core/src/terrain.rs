//! The land the kingdom stands on.
//!
//! This exists to answer a complaint that sounds aesthetic but is structural:
//! the map used to be cities floating in a void. Each city was drawn in
//! isometric while the world holding them was a flat scatter, so every project
//! read as a diorama pinned to a wall rather than a settlement in a place.
//!
//! Terrain fixes that by giving the kingdom a **ground plane with edges**: a
//! coastline, a sea beyond it, and relief in between. Once land ends somewhere,
//! everything standing on it starts reading as *in* a world instead of *on* a
//! background.
//!
//! ## Two properties this module owes the rest of the map
//!
//! - **Purity.** Same input, same continent, on every machine and every reload.
//!   The noise is built from wrapping integer hashes rather than anything
//!   floating-point-order dependent, so it is reproducible everywhere
//!   `kingdom-core` compiles -- native and wasm alike.
//! - **Independence from the kingdom's contents.** The shape is seeded from the
//!   kingdom's path *alone*. Scan a folder today and in six months and the
//!   coastline is identical, however many projects came and went. A world that
//!   redraws itself when you `cargo new` is not a world, and the King's spatial
//!   memory -- the entire reason a map beats a list -- would be worthless.
//!
//! ## Terrain is decoration, and is held to decoration's rules
//!
//! `AGENTS.md` insists that colour on screen should mean something. Elevation
//! noise means nothing, and this module does not pretend otherwise. It earns its
//! place as *substrate*: it exists so that the things which do carry meaning --
//! a gilded roof, a blocked architect, a contended port -- read as objects
//! standing in a place. The discipline that keeps that honest lives in the
//! stylesheet: terrain may only use desaturated slates and blues, and must never
//! approach the saturation of a [`crate::Language`] tint or a status colour. If the
//! terrain ever competes with a signal for attention, the terrain is wrong.

// ---------------------------------------------------------------------------
// Shape constants
// ---------------------------------------------------------------------------

/// How much of the elevation comes from the island dome rather than the noise.
///
/// The dome is what closes the landmass into an island instead of letting it
/// run off the viewport forever.
const DOME_WEIGHT: f64 = 0.8;

/// How much of the elevation comes from the noise field.
const NOISE_WEIGHT: f64 = 0.75;

/// Exponent of the dome falloff.
///
/// Deliberately steep. A gentle (quadratic) falloff makes elevation mostly a
/// function of distance from the centre, and the island comes out as a bullseye
/// of concentric rings -- the most artificial-looking thing this module could
/// possibly produce. At this exponent the dome stays near 1 across the whole
/// interior, so relief there is driven by the noise and reads as organic, and
/// then plunges near the rim to force a closed coastline.
const DOME_FALLOFF: f64 = 3.5;

/// How far out the sampling grid reaches, as a multiple of `extent`.
///
/// Chosen so the field is comfortably below sea level along the whole grid
/// border. That is what guarantees every contour ring closes *inside* the grid,
/// which in turn means [`contours`] never has to stitch a ring along the border
/// -- the fiddliest and most bug-prone part of marching squares, avoided
/// outright rather than implemented carefully.
const SAMPLE_SPAN: f64 = 1.35;

/// Sea level. Elevation is signed, so this is simply zero, but naming it makes
/// the comparisons at the call sites read as geography rather than arithmetic.
pub const SEA_LEVEL: f64 = 0.0;

/// Elevation thresholds for the drawn bands, from shore to peak.
///
/// Five bands is enough to read as relief and few enough to stay quiet.
///
/// The values are fitted to the field's *measured* range rather than guessed.
/// Land runs from 0 to roughly 0.9, and the peak varies with the seed (0.86 to
/// 1.06 across sampled kingdoms) -- so a band up at 0.86, which is where these
/// started, enclosed nothing at all on most kingdoms and silently cost a
/// contour pass per render. The top band sits at 0.70 so every kingdom actually
/// grows highlands.
pub const BANDS: [f64; 5] = [SEA_LEVEL, 0.16, 0.34, 0.52, 0.70];

/// How much of the island's extent the highest ground rises by.
///
/// Deliberately slight. Relief exists to stop the ground reading as a flat
/// sheet, not to build mountains: too much and a city on a peak floats visibly
/// above its neighbours, and the isometric depth cues start fighting the
/// back-to-front paint order instead of reinforcing it.
const RELIEF: f64 = 0.05;

// ---------------------------------------------------------------------------
// The terrain field
// ---------------------------------------------------------------------------

/// A deterministic elevation field: one island, centred on the throne.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Terrain {
    seed: u64,
    extent: f64,
}

impl Terrain {
    /// Builds the continent for a kingdom.
    ///
    /// `root` is the kingdom's path and is the *only* thing that seeds the
    /// shape; `extent` scales it to fit however many cities need to stand on it.
    pub fn for_kingdom(root: &str, extent: f64) -> Terrain {
        Terrain {
            seed: hash_str(root),
            extent: extent.max(1.0),
        }
    }

    /// Distance from the throne at which the dome reaches sea level.
    pub fn extent(&self) -> f64 {
        self.extent
    }

    /// Half-width of the square this terrain is sampled over.
    ///
    /// The sea should be painted at least this far out, or the ocean visibly
    /// stops short of the island's own horizon.
    pub fn span(&self) -> f64 {
        self.extent * SAMPLE_SPAN
    }

    /// Ground height at a point. Below [`SEA_LEVEL`] is water.
    pub fn elevation(&self, x: f64, y: f64) -> f64 {
        let d = (x * x + y * y).sqrt() / self.extent;
        let dome = 1.0 - d.powf(DOME_FALLOFF);
        dome * DOME_WEIGHT + (self.fbm(x, y) - 0.5) * NOISE_WEIGHT
    }

    pub fn is_land(&self, x: f64, y: f64) -> bool {
        self.elevation(x, y) > SEA_LEVEL
    }

    /// Ground height in **world units**, for standing things on the terrain.
    ///
    /// [`Self::elevation`] is the raw signed field, which is convenient for
    /// contouring but is roughly `-1..1` and so useless as a coordinate. This
    /// scales it into the same units the rest of the map measures in, and
    /// flattens the sea to zero so a coastal city never sinks below the water
    /// it stands beside.
    pub fn height(&self, x: f64, y: f64) -> f64 {
        self.elevation(x, y).max(SEA_LEVEL) * self.extent * RELIEF
    }

    /// A stable pseudo-random value in `0.0..1.0` for a lattice cell.
    ///
    /// Exposed so that [`crate::layout`] can jitter its settlement lattice from
    /// the *terrain's* seed rather than inventing its own. That matters: the
    /// jitter is then a property of the land, so it cannot shift when the
    /// kingdom's contents change.
    pub fn jitter(&self, ix: i64, iy: i64) -> f64 {
        hash_cell(self.seed ^ 0x5DEE_CE66_D1B0_7C15, ix, iy)
    }

    /// A fixed rotation for this kingdom, in radians.
    ///
    /// Lets provinces sit at a different bearing in every kingdom, so two
    /// different dev folders do not both put Rust in the north.
    pub fn rotation(&self) -> f64 {
        (self.seed >> 11) as f64 / ((1u64 << 53) as f64) * std::f64::consts::TAU
    }

    /// Fractal Brownian motion over value noise, in `0.0..=1.0`.
    ///
    /// Four octaves: enough for headlands and bays at a glance plus texture up
    /// close, without the cost of octaves finer than the contour grid can
    /// resolve anyway.
    fn fbm(&self, x: f64, y: f64) -> f64 {
        let mut freq = 1.0 / (self.extent * 0.62);
        let mut amp = 1.0;
        let mut sum = 0.0;
        let mut norm = 0.0;

        for octave in 0..4u64 {
            let seed = self.seed ^ octave.wrapping_mul(0x9E37_79B9_7F4A_7C15);
            sum += amp * value_noise(seed, x * freq, y * freq);
            norm += amp;
            amp *= 0.5;
            freq *= 2.0;
        }

        sum / norm
    }
}

/// FNV-1a. Small, dependency-free, and identical on every target -- which is
/// the whole requirement, since this seed decides the shape of the world.
fn hash_str(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    // A kingdom at the filesystem root would otherwise seed from an empty
    // string; the offset basis alone is a perfectly good island.
    h
}

/// Hashes a lattice cell to `0.0..=1.0`.
///
/// Integer-only mixing (splitmix64's finaliser) so the field is bit-identical
/// across platforms rather than merely similar.
fn hash_cell(seed: u64, ix: i64, iy: i64) -> f64 {
    let mut h = seed;
    h ^= (ix as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    h = h.rotate_left(31);
    h ^= (iy as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    h ^= h >> 30;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 27;
    h = h.wrapping_mul(0x94D0_49BB_1331_11EB);
    h ^= h >> 31;
    // Top 53 bits: every f64 in the unit interval, uniformly.
    (h >> 11) as f64 / ((1u64 << 53) as f64)
}

/// Value noise with smoothstep interpolation.
fn value_noise(seed: u64, x: f64, y: f64) -> f64 {
    let fx = x.floor();
    let fy = y.floor();
    let (ix, iy) = (fx as i64, fy as i64);
    let (tx, ty) = (x - fx, y - fy);

    // Smoothstep, so the lattice does not show up as a visible grid of creases.
    let sx = tx * tx * (3.0 - 2.0 * tx);
    let sy = ty * ty * (3.0 - 2.0 * ty);

    let n00 = hash_cell(seed, ix, iy);
    let n10 = hash_cell(seed, ix + 1, iy);
    let n01 = hash_cell(seed, ix, iy + 1);
    let n11 = hash_cell(seed, ix + 1, iy + 1);

    let top = n00 + (n10 - n00) * sx;
    let bottom = n01 + (n11 - n01) * sx;
    top + (bottom - top) * sy
}

// ---------------------------------------------------------------------------
// Contours
// ---------------------------------------------------------------------------

/// One elevation band, as closed rings in ground coordinates.
#[derive(Debug, Clone, PartialEq)]
pub struct Band {
    pub threshold: f64,
    /// Closed rings enclosing everything at or above `threshold`.
    ///
    /// Render all of a band's rings as a **single** path with
    /// `fill-rule="evenodd"`. Nested rings then punch themselves out as lakes
    /// and inlets automatically, which is why nothing here has to care about
    /// winding order.
    pub rings: Vec<Vec<(f64, f64)>>,
}

/// Traces closed contour rings for each threshold.
///
/// Marching squares over a single shared sample grid. The alternative -- one
/// quad per cell, coloured by band -- would mean thousands of SVG nodes per
/// kingdom; contours give a handful of paths, and give *curves* rather than a
/// staircase, which is most of what makes the coast read as a coast.
///
/// `res` is the number of cells per side. Cost is `O(res^2)` samples, taken
/// once and reused for every threshold.
pub fn contours(terrain: &Terrain, thresholds: &[f64], res: usize) -> Vec<Band> {
    let res = res.max(4);
    let span = terrain.span();
    let step = span * 2.0 / res as f64;
    let origin = -span;

    // Sample once; every threshold walks the same field.
    let mut field = vec![0.0f64; (res + 1) * (res + 1)];
    for r in 0..=res {
        for c in 0..=res {
            let x = origin + c as f64 * step;
            let y = origin + r as f64 * step;
            field[r * (res + 1) + c] = terrain.elevation(x, y);
        }
    }

    thresholds
        .iter()
        .map(|&threshold| Band {
            threshold,
            rings: trace(&field, res, origin, step, threshold),
        })
        .collect()
}

/// A crossing point on one grid edge, and the up-to-two crossings it links to.
#[derive(Clone, Copy)]
struct Crossing {
    pos: (f64, f64),
    links: [usize; 2],
    count: u8,
}

/// Marching squares at one threshold.
fn trace(
    field: &[f64],
    res: usize,
    origin: f64,
    step: f64,
    threshold: f64,
) -> Vec<Vec<(f64, f64)>> {
    let stride = res + 1;
    let at = |r: usize, c: usize| field[r * stride + c];

    // Every crossing lies on exactly one grid edge, so edges -- not coordinates
    // -- are the identity used to stitch segments into rings. Matching on
    // integer edge ids instead of floating-point positions removes the whole
    // category of "two endpoints that should be equal differ in the last bit".
    let horizontal = stride * res; // horizontal edges: (res+1) rows x res cols
    let total = horizontal + res * stride;
    let mut nodes: Vec<Option<Crossing>> = vec![None; total];

    let h_edge = |r: usize, c: usize| r * res + c;
    let v_edge = |r: usize, c: usize| horizontal + r * stride + c;

    let link = |nodes: &mut Vec<Option<Crossing>>, edge: usize, pos: (f64, f64), other: usize| {
        let node = nodes[edge].get_or_insert(Crossing {
            pos,
            links: [usize::MAX; 2],
            count: 0,
        });
        if node.count < 2 {
            node.links[node.count as usize] = other;
            node.count += 1;
        }
    };

    for r in 0..res {
        for c in 0..res {
            let tl = at(r, c);
            let tr = at(r, c + 1);
            let br = at(r + 1, c + 1);
            let bl = at(r + 1, c);

            let mut case = 0u8;
            if tl >= threshold {
                case |= 8;
            }
            if tr >= threshold {
                case |= 4;
            }
            if br >= threshold {
                case |= 2;
            }
            if bl >= threshold {
                case |= 1;
            }

            if case == 0 || case == 15 {
                continue;
            }

            // Positions of the four possible crossings on this cell's edges.
            let x0 = origin + c as f64 * step;
            let y0 = origin + r as f64 * step;
            let top = (x0 + lerp_frac(tl, tr, threshold) * step, y0);
            let bottom = (x0 + lerp_frac(bl, br, threshold) * step, y0 + step);
            let left = (x0, y0 + lerp_frac(tl, bl, threshold) * step);
            let right = (x0 + step, y0 + lerp_frac(tr, br, threshold) * step);

            const T: usize = 0;
            const R: usize = 1;
            const B: usize = 2;
            const L: usize = 3;

            // Saddles (5 and 10) are genuinely ambiguous: the cell corners alone
            // do not say whether the high ground is joined or pinched. The cell
            // centre breaks the tie, which is the standard resolution and the
            // one that keeps a narrow isthmus looking like an isthmus.
            let centre = (tl + tr + br + bl) / 4.0;
            let pairs: [(usize, usize); 2] = match case {
                1 | 14 => [(L, B), (usize::MAX, usize::MAX)],
                2 | 13 => [(B, R), (usize::MAX, usize::MAX)],
                3 | 12 => [(L, R), (usize::MAX, usize::MAX)],
                4 | 11 => [(T, R), (usize::MAX, usize::MAX)],
                6 | 9 => [(T, B), (usize::MAX, usize::MAX)],
                7 | 8 => [(T, L), (usize::MAX, usize::MAX)],
                5 => {
                    if centre >= threshold {
                        [(T, L), (B, R)]
                    } else {
                        [(T, R), (L, B)]
                    }
                }
                10 => {
                    if centre >= threshold {
                        [(T, R), (L, B)]
                    } else {
                        [(T, L), (B, R)]
                    }
                }
                _ => [(usize::MAX, usize::MAX), (usize::MAX, usize::MAX)],
            };

            for (a, b) in pairs {
                if a == usize::MAX {
                    continue;
                }
                let id = |side: usize| match side {
                    T => (h_edge(r, c), top),
                    B => (h_edge(r + 1, c), bottom),
                    L => (v_edge(r, c), left),
                    _ => (v_edge(r, c + 1), right),
                };
                let (ea, pa) = id(a);
                let (eb, pb) = id(b);
                link(&mut nodes, ea, pa, eb);
                link(&mut nodes, eb, pb, ea);
            }
        }
    }

    // Walk each cycle exactly once. Every ring closes inside the grid, because
    // SAMPLE_SPAN puts the whole border below the lowest threshold.
    let mut seen = vec![false; total];
    let mut rings = Vec::new();

    for start in 0..total {
        if seen[start] || nodes[start].is_none() {
            continue;
        }

        let mut ring: Vec<(f64, f64)> = Vec::new();
        let mut current = start;
        let mut previous = usize::MAX;

        loop {
            seen[current] = true;
            let node = match nodes[current] {
                Some(n) => n,
                None => break,
            };
            ring.push(node.pos);

            let mut next = usize::MAX;
            for i in 0..node.count as usize {
                let candidate = node.links[i];
                if candidate != previous {
                    next = candidate;
                    break;
                }
            }

            if next == usize::MAX || next == start || seen[next] {
                break;
            }
            previous = current;
            current = next;
        }

        // Two-point "rings" are a single stray segment, not a region.
        if ring.len() < 3 {
            continue;
        }

        let smoothed = smooth_closed(&ring, 2);
        let simplified = simplify_closed(&smoothed, step * 0.3);
        if simplified.len() >= 3 {
            rings.push(simplified);
        }
    }

    rings
}

/// Where between `a` and `b` the threshold falls, clamped to the edge.
fn lerp_frac(a: f64, b: f64, threshold: f64) -> f64 {
    let d = b - a;
    if d.abs() < f64::EPSILON {
        0.5
    } else {
        ((threshold - a) / d).clamp(0.0, 1.0)
    }
}

/// Chaikin corner cutting on a closed ring.
///
/// Marching squares emits polylines that follow the sample grid, so raw output
/// is faintly staircased -- the exact machine-made look this whole task exists
/// to get away from. Two rounds of corner cutting turn it into a smooth coast
/// far more cheaply than quadrupling the grid resolution would.
fn smooth_closed(ring: &[(f64, f64)], rounds: usize) -> Vec<(f64, f64)> {
    let mut points = ring.to_vec();

    for _ in 0..rounds {
        if points.len() < 3 {
            break;
        }
        let mut next = Vec::with_capacity(points.len() * 2);
        for i in 0..points.len() {
            let a = points[i];
            let b = points[(i + 1) % points.len()];
            next.push((a.0 * 0.75 + b.0 * 0.25, a.1 * 0.75 + b.1 * 0.25));
            next.push((a.0 * 0.25 + b.0 * 0.75, a.1 * 0.25 + b.1 * 0.75));
        }
        points = next;
    }

    points
}

/// Drops points closer together than `min_distance`.
///
/// Smoothing leaves dense clusters of near-identical points on straight runs;
/// they cost path bytes and buy nothing at any zoom the map offers.
fn simplify_closed(ring: &[(f64, f64)], min_distance: f64) -> Vec<(f64, f64)> {
    let mut out: Vec<(f64, f64)> = Vec::with_capacity(ring.len());
    let min2 = min_distance * min_distance;

    for &p in ring {
        match out.last() {
            Some(&last) => {
                let (dx, dy) = (p.0 - last.0, p.1 - last.1);
                if dx * dx + dy * dy >= min2 {
                    out.push(p);
                }
            }
            None => out.push(p),
        }
    }

    // The ring closes, so the last point must also clear the first.
    if out.len() >= 2 {
        let first = out[0];
        let last = out[out.len() - 1];
        let (dx, dy) = (first.0 - last.0, first.1 - last.1);
        if dx * dx + dy * dy < min2 {
            out.pop();
        }
    }

    out
}
