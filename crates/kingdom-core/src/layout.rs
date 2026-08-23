//! Deterministic placement of cities on the kingdom map.
//!
//! Layout lives in `kingdom-core`, not in the UI, for one reason: it must be
//! **stable**. A city has to land in the same spot on every render and every
//! reload, or the King loses the spatial memory that makes a map worth having
//! at all. Keeping it as a pure function of the city list makes that testable.

use crate::model::City;

/// A city's computed position on the map, in abstract world units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CityPlacement {
    pub x: f64,
    pub y: f64,
    /// Radius of the city's footprint, scaled by project size.
    pub radius: f64,
}

/// The golden angle, in radians.
///
/// Successive multiples of this angle never repeat, which is what stops a
/// spiral layout from forming visible spokes.
const GOLDEN_ANGLE: f64 = 2.399_963_229_728_653;

/// Distance between successive rings of the spiral.
const RING_SPACING: f64 = 190.0;

/// Places cities in a phyllotactic (sunflower) spiral around the throne.
///
/// The spiral gives even spacing with no clustering and no fixed grid, scales
/// gracefully from 3 projects to 300, and — being index-based — is entirely
/// deterministic. Cities are placed in list order, so callers should sort
/// first if they want a stable arrangement as projects come and go.
pub fn spiral_layout(cities: &[City]) -> Vec<CityPlacement> {
    cities
        .iter()
        .enumerate()
        .map(|(i, city)| {
            // The +0.5 offset keeps the first city off the exact centre, which
            // is reserved for the throne.
            let index = i as f64 + 0.5;
            let angle = index * GOLDEN_ANGLE;
            let distance = RING_SPACING * index.sqrt();

            CityPlacement {
                x: distance * angle.cos(),
                y: distance * angle.sin(),
                radius: city_radius(city),
            }
        })
        .collect()
}

/// Scales a city's footprint by file count, compressed logarithmically so a
/// monorepo does not dwarf a small library into invisibility.
fn city_radius(city: &City) -> f64 {
    let base = 38.0;
    let growth = ((city.file_count as f64).max(1.0)).ln() * 7.0;
    (base + growth).clamp(38.0, 96.0)
}

/// The bounding box of a laid-out kingdom, used to frame the initial view.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bounds {
    pub min_x: f64,
    pub min_y: f64,
    pub max_x: f64,
    pub max_y: f64,
}

impl Bounds {
    pub fn width(&self) -> f64 {
        self.max_x - self.min_x
    }

    pub fn height(&self) -> f64 {
        self.max_y - self.min_y
    }

    pub fn center(&self) -> (f64, f64) {
        (
            (self.min_x + self.max_x) / 2.0,
            (self.min_y + self.max_y) / 2.0,
        )
    }
}

/// Computes the extent of a set of placements, including each city's radius
/// and a margin so nothing is flush against the viewport edge.
pub fn bounds_of(placements: &[CityPlacement]) -> Bounds {
    if placements.is_empty() {
        return Bounds {
            min_x: -400.0,
            min_y: -300.0,
            max_x: 400.0,
            max_y: 300.0,
        };
    }

    let margin = 140.0;
    let mut b = Bounds {
        min_x: f64::MAX,
        min_y: f64::MAX,
        max_x: f64::MIN,
        max_y: f64::MIN,
    };

    for p in placements {
        b.min_x = b.min_x.min(p.x - p.radius - margin);
        b.min_y = b.min_y.min(p.y - p.radius - margin);
        b.max_x = b.max_x.max(p.x + p.radius + margin);
        b.max_y = b.max_y.max(p.y + p.radius + margin);
    }

    b
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::CityId;
    use crate::model::CityKind;

    fn city(name: &str, files: usize) -> City {
        City {
            id: CityId::new(name),
            name: name.into(),
            path: name.into(),
            kind: CityKind::Unknown,
            file_count: files,
            has_git: false,
            dirty_files: 0,
        }
    }

    /// Cities must never overlap, or the map becomes unreadable at exactly the
    /// moment it matters most: when the kingdom is large.
    #[test]
    fn spiral_layout_never_overlaps_cities() {
        let cities: Vec<City> = (0..60).map(|i| city(&format!("c{i}"), 5_000)).collect();
        let placements = spiral_layout(&cities);

        for (i, a) in placements.iter().enumerate() {
            for (j, b) in placements.iter().enumerate().skip(i + 1) {
                let distance = ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt();
                assert!(
                    distance > a.radius + b.radius,
                    "cities {i} and {j} overlap: gap {distance:.1} <= radii {:.1}",
                    a.radius + b.radius
                );
            }
        }
    }

    /// The King's spatial memory depends on a city not moving between reloads.
    #[test]
    fn spiral_layout_is_deterministic() {
        let cities = vec![city("a", 10), city("b", 200), city("c", 3)];
        assert_eq!(spiral_layout(&cities), spiral_layout(&cities));
    }
}
