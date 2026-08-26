//! Procedural ground-painted text for ward and folder names.
//!
//! Repo City labels some plazas directly on the ground, where Bevy can light,
//! cull, and sort them with the rest of the settlement. Shipping a font file
//! would add an asset pipeline to a renderer that otherwise builds its map from
//! geometry, so this module uses a small single-stroke, Hershey-style alphabet
//! and turns each stroke into a flat ribbon on the ground plane.

use bevy::prelude::*;

use super::meshes::MeshBuilder;

#[derive(Clone, Copy)]
struct Glyph {
    advance: f32,
    strokes: &'static [&'static [(f32, f32)]],
}

/// The advance width of `text` in em units, so a caller can size text to fit a space.
pub fn text_width(text: &str) -> f32 {
    if text.trim().is_empty() {
        return 0.0;
    }
    text.chars().map(|character| glyph(character).advance).sum()
}

/// A flat, ground-plane (y = 0) mesh of `text`.
///
/// `size` is the cap height in world units and `stroke` is the stroke width in
/// world units. The text starts at the local origin and runs along +x with its
/// caps rising toward -z, sitting on the baseline, so the caller places it with
/// a Transform. The cap direction is -z rather than +z because the isometric
/// camera puts -x/-z at the top of the screen: text drawn the other way up
/// reads upside down.
pub fn text_mesh(text: &str, size: f32, stroke: f32) -> Mesh {
    let mut builder = MeshBuilder::new();
    if text.trim().is_empty() || size <= 0.0 || stroke <= 0.0 {
        return builder.build();
    }

    let mut pen = 0.0;
    for character in text.chars() {
        let glyph = glyph(character);
        for stroke_points in glyph.strokes {
            for segment in stroke_points.windows(2) {
                add_segment(
                    &mut builder,
                    Vec2::new(pen + segment[0].0 * size, segment[0].1 * size),
                    Vec2::new(pen + segment[1].0 * size, segment[1].1 * size),
                    stroke,
                );
            }
        }
        pen += glyph.advance * size;
    }

    builder.build()
}

/// Lays one stroke down as a ribbon quad on the ground plane.
///
/// Glyphs are authored with y running up the em box; the ground plane's second
/// axis is z, and z grows toward the bottom of the screen, so the glyph is
/// mirrored onto -z here. Mirroring reverses triangle orientation, and Bevy
/// takes facing from index winding, so the corners are emitted in the opposite
/// order to keep every quad facing the sky.
fn add_segment(builder: &mut MeshBuilder, from: Vec2, to: Vec2, width: f32) {
    let half = width * 0.5;
    let direction = (to - from).normalize_or_zero();
    if direction == Vec2::ZERO {
        return;
    }

    let from = from - direction * half;
    let to = to + direction * half;
    let side = Vec2::new(-direction.y, direction.x) * half;
    let ground = |point: Vec2| Vec3::new(point.x, 0.0, -point.y);
    builder.quad(
        ground(to - side),
        ground(to + side),
        ground(from + side),
        ground(from - side),
    );
}

fn glyph(character: char) -> Glyph {
    match character.to_ascii_uppercase() {
        'A' => Glyph {
            advance: 0.72,
            strokes: &[
                &[(0.05, 0.0), (0.22, 1.0), (0.50, 1.0), (0.67, 0.0)],
                &[(0.14, 0.45), (0.58, 0.45)],
            ],
        },
        'B' => Glyph {
            advance: 0.68,
            strokes: &[
                &[
                    (0.06, 0.0),
                    (0.06, 1.0),
                    (0.42, 1.0),
                    (0.60, 0.85),
                    (0.60, 0.58),
                    (0.42, 0.50),
                    (0.06, 0.50),
                ],
                &[
                    (0.42, 0.50),
                    (0.62, 0.38),
                    (0.62, 0.12),
                    (0.42, 0.0),
                    (0.06, 0.0),
                ],
            ],
        },
        'C' => Glyph {
            advance: 0.68,
            strokes: &[&[
                (0.62, 0.82),
                (0.48, 1.0),
                (0.18, 1.0),
                (0.05, 0.82),
                (0.05, 0.18),
                (0.18, 0.0),
                (0.48, 0.0),
                (0.62, 0.18),
            ]],
        },
        'D' => Glyph {
            advance: 0.70,
            strokes: &[&[
                (0.06, 0.0),
                (0.06, 1.0),
                (0.42, 1.0),
                (0.64, 0.78),
                (0.64, 0.22),
                (0.42, 0.0),
                (0.06, 0.0),
            ]],
        },
        'E' => Glyph {
            advance: 0.62,
            strokes: &[
                &[(0.56, 1.0), (0.06, 1.0), (0.06, 0.0), (0.56, 0.0)],
                &[(0.06, 0.50), (0.48, 0.50)],
            ],
        },
        'F' => Glyph {
            advance: 0.60,
            strokes: &[
                &[(0.06, 0.0), (0.06, 1.0), (0.56, 1.0)],
                &[(0.06, 0.50), (0.46, 0.50)],
            ],
        },
        'G' => Glyph {
            advance: 0.70,
            strokes: &[&[
                (0.64, 0.82),
                (0.50, 1.0),
                (0.18, 1.0),
                (0.05, 0.82),
                (0.05, 0.18),
                (0.18, 0.0),
                (0.50, 0.0),
                (0.64, 0.16),
                (0.64, 0.42),
                (0.42, 0.42),
            ]],
        },
        'H' => Glyph {
            advance: 0.70,
            strokes: &[
                &[(0.06, 0.0), (0.06, 1.0)],
                &[(0.64, 0.0), (0.64, 1.0)],
                &[(0.06, 0.50), (0.64, 0.50)],
            ],
        },
        'I' => Glyph {
            advance: 0.34,
            strokes: &[
                &[(0.06, 1.0), (0.28, 1.0)],
                &[(0.17, 1.0), (0.17, 0.0)],
                &[(0.06, 0.0), (0.28, 0.0)],
            ],
        },
        'J' => Glyph {
            advance: 0.54,
            strokes: &[
                &[(0.08, 1.0), (0.48, 1.0)],
                &[
                    (0.36, 1.0),
                    (0.36, 0.18),
                    (0.24, 0.0),
                    (0.08, 0.0),
                    (0.04, 0.18),
                ],
            ],
        },
        'K' => Glyph {
            advance: 0.68,
            strokes: &[
                &[(0.06, 0.0), (0.06, 1.0)],
                &[(0.62, 1.0), (0.06, 0.45), (0.62, 0.0)],
            ],
        },
        'L' => Glyph {
            advance: 0.58,
            strokes: &[&[(0.06, 1.0), (0.06, 0.0), (0.54, 0.0)]],
        },
        'M' => Glyph {
            advance: 0.86,
            strokes: &[&[
                (0.06, 0.0),
                (0.06, 1.0),
                (0.43, 0.42),
                (0.80, 1.0),
                (0.80, 0.0),
            ]],
        },
        'N' => Glyph {
            advance: 0.74,
            strokes: &[&[(0.06, 0.0), (0.06, 1.0), (0.68, 0.0), (0.68, 1.0)]],
        },
        'O' => Glyph {
            advance: 0.72,
            strokes: &[&[
                (0.20, 0.0),
                (0.52, 0.0),
                (0.67, 0.18),
                (0.67, 0.82),
                (0.52, 1.0),
                (0.20, 1.0),
                (0.05, 0.82),
                (0.05, 0.18),
                (0.20, 0.0),
            ]],
        },
        'P' => Glyph {
            advance: 0.66,
            strokes: &[&[
                (0.06, 0.0),
                (0.06, 1.0),
                (0.44, 1.0),
                (0.62, 0.82),
                (0.62, 0.58),
                (0.44, 0.42),
                (0.06, 0.42),
            ]],
        },
        'Q' => Glyph {
            advance: 0.74,
            strokes: &[
                &[
                    (0.20, 0.0),
                    (0.52, 0.0),
                    (0.67, 0.18),
                    (0.67, 0.82),
                    (0.52, 1.0),
                    (0.20, 1.0),
                    (0.05, 0.82),
                    (0.05, 0.18),
                    (0.20, 0.0),
                ],
                &[(0.45, 0.25), (0.70, -0.06)],
            ],
        },
        'R' => Glyph {
            advance: 0.70,
            strokes: &[
                &[
                    (0.06, 0.0),
                    (0.06, 1.0),
                    (0.46, 1.0),
                    (0.64, 0.82),
                    (0.64, 0.58),
                    (0.46, 0.42),
                    (0.06, 0.42),
                ],
                &[(0.40, 0.42), (0.66, 0.0)],
            ],
        },
        'S' => Glyph {
            advance: 0.66,
            strokes: &[&[
                (0.60, 0.84),
                (0.46, 1.0),
                (0.16, 1.0),
                (0.05, 0.84),
                (0.17, 0.58),
                (0.48, 0.44),
                (0.61, 0.18),
                (0.48, 0.0),
                (0.16, 0.0),
                (0.04, 0.16),
            ]],
        },
        'T' => Glyph {
            advance: 0.66,
            strokes: &[&[(0.05, 1.0), (0.61, 1.0)], &[(0.33, 1.0), (0.33, 0.0)]],
        },
        'U' => Glyph {
            advance: 0.72,
            strokes: &[&[
                (0.06, 1.0),
                (0.06, 0.18),
                (0.20, 0.0),
                (0.52, 0.0),
                (0.66, 0.18),
                (0.66, 1.0),
            ]],
        },
        'V' => Glyph {
            advance: 0.72,
            strokes: &[&[(0.05, 1.0), (0.36, 0.0), (0.67, 1.0)]],
        },
        'W' => Glyph {
            advance: 0.90,
            strokes: &[&[
                (0.05, 1.0),
                (0.22, 0.0),
                (0.45, 0.55),
                (0.68, 0.0),
                (0.85, 1.0),
            ]],
        },
        'X' => Glyph {
            advance: 0.70,
            strokes: &[&[(0.06, 1.0), (0.64, 0.0)], &[(0.64, 1.0), (0.06, 0.0)]],
        },
        'Y' => Glyph {
            advance: 0.70,
            strokes: &[
                &[(0.05, 1.0), (0.35, 0.52), (0.65, 1.0)],
                &[(0.35, 0.52), (0.35, 0.0)],
            ],
        },
        'Z' => Glyph {
            advance: 0.66,
            strokes: &[&[(0.05, 1.0), (0.61, 1.0), (0.05, 0.0), (0.61, 0.0)]],
        },
        '0' => Glyph {
            advance: 0.66,
            strokes: &[
                &[
                    (0.18, 0.0),
                    (0.48, 0.0),
                    (0.61, 0.18),
                    (0.61, 0.82),
                    (0.48, 1.0),
                    (0.18, 1.0),
                    (0.05, 0.82),
                    (0.05, 0.18),
                    (0.18, 0.0),
                ],
                &[(0.50, 0.85), (0.16, 0.15)],
            ],
        },
        '1' => Glyph {
            advance: 0.38,
            strokes: &[
                &[(0.08, 0.78), (0.20, 1.0), (0.20, 0.0)],
                &[(0.08, 0.0), (0.32, 0.0)],
            ],
        },
        '2' => Glyph {
            advance: 0.62,
            strokes: &[&[
                (0.06, 0.78),
                (0.18, 1.0),
                (0.46, 1.0),
                (0.58, 0.80),
                (0.06, 0.0),
                (0.58, 0.0),
            ]],
        },
        '3' => Glyph {
            advance: 0.62,
            strokes: &[&[
                (0.06, 1.0),
                (0.56, 1.0),
                (0.34, 0.54),
                (0.56, 0.42),
                (0.56, 0.16),
                (0.42, 0.0),
                (0.14, 0.0),
                (0.04, 0.16),
            ]],
        },
        '4' => Glyph {
            advance: 0.66,
            strokes: &[&[(0.50, 0.0), (0.50, 1.0), (0.05, 0.34), (0.62, 0.34)]],
        },
        '5' => Glyph {
            advance: 0.62,
            strokes: &[&[
                (0.56, 1.0),
                (0.10, 1.0),
                (0.06, 0.55),
                (0.42, 0.55),
                (0.58, 0.38),
                (0.58, 0.16),
                (0.44, 0.0),
                (0.14, 0.0),
                (0.04, 0.14),
            ]],
        },
        '6' => Glyph {
            advance: 0.64,
            strokes: &[&[
                (0.56, 0.84),
                (0.44, 1.0),
                (0.18, 1.0),
                (0.05, 0.70),
                (0.05, 0.18),
                (0.20, 0.0),
                (0.46, 0.0),
                (0.59, 0.18),
                (0.59, 0.42),
                (0.44, 0.58),
                (0.05, 0.58),
            ]],
        },
        '7' => Glyph {
            advance: 0.60,
            strokes: &[&[(0.04, 1.0), (0.56, 1.0), (0.22, 0.0)]],
        },
        '8' => Glyph {
            advance: 0.66,
            strokes: &[
                &[
                    (0.18, 0.0),
                    (0.48, 0.0),
                    (0.60, 0.16),
                    (0.60, 0.36),
                    (0.48, 0.50),
                    (0.18, 0.50),
                    (0.06, 0.36),
                    (0.06, 0.16),
                    (0.18, 0.0),
                ],
                &[
                    (0.18, 0.50),
                    (0.06, 0.64),
                    (0.06, 0.84),
                    (0.18, 1.0),
                    (0.48, 1.0),
                    (0.60, 0.84),
                    (0.60, 0.64),
                    (0.48, 0.50),
                ],
            ],
        },
        '9' => Glyph {
            advance: 0.64,
            strokes: &[&[
                (0.58, 0.42),
                (0.20, 0.42),
                (0.06, 0.58),
                (0.06, 0.82),
                (0.20, 1.0),
                (0.46, 1.0),
                (0.59, 0.82),
                (0.59, 0.30),
                (0.46, 0.0),
                (0.18, 0.0),
                (0.06, 0.16),
            ]],
        },
        ' ' => Glyph {
            advance: 0.35,
            strokes: &[],
        },
        '.' => Glyph {
            advance: 0.24,
            strokes: &[&[(0.08, 0.02), (0.16, 0.02)]],
        },
        '-' => Glyph {
            advance: 0.42,
            strokes: &[&[(0.06, 0.48), (0.36, 0.48)]],
        },
        '_' => Glyph {
            advance: 0.56,
            strokes: &[&[(0.04, -0.08), (0.52, -0.08)]],
        },
        '/' => Glyph {
            advance: 0.50,
            strokes: &[&[(0.06, 0.0), (0.44, 1.0)]],
        },
        '+' => Glyph {
            advance: 0.54,
            strokes: &[&[(0.07, 0.50), (0.47, 0.50)], &[(0.27, 0.25), (0.27, 0.75)]],
        },
        '(' => Glyph {
            advance: 0.36,
            strokes: &[&[
                (0.30, 1.0),
                (0.12, 0.80),
                (0.08, 0.50),
                (0.12, 0.20),
                (0.30, 0.0),
            ]],
        },
        ')' => Glyph {
            advance: 0.36,
            strokes: &[&[
                (0.06, 1.0),
                (0.24, 0.80),
                (0.28, 0.50),
                (0.24, 0.20),
                (0.06, 0.0),
            ]],
        },
        '…' => Glyph {
            advance: 0.62,
            strokes: &[
                &[(0.08, 0.02), (0.16, 0.02)],
                &[(0.27, 0.02), (0.35, 0.02)],
                &[(0.46, 0.02), (0.54, 0.02)],
            ],
        },
        _ => placeholder_glyph(),
    }
}

fn placeholder_glyph() -> Glyph {
    Glyph {
        advance: 0.62,
        strokes: &[
            &[
                (0.06, 0.0),
                (0.06, 1.0),
                (0.56, 1.0),
                (0.56, 0.0),
                (0.06, 0.0),
            ],
            &[(0.12, 0.18), (0.50, 0.82)],
            &[(0.50, 0.18), (0.12, 0.82)],
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::mesh::Indices;

    const VISIBLE_SUPPORTED: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789.-_/+()…";

    fn triangles(mesh: &Mesh) -> Vec<[Vec3; 3]> {
        let positions = mesh
            .attribute(Mesh::ATTRIBUTE_POSITION)
            .and_then(|values| values.as_float3())
            .expect("mesh has positions");
        let Some(Indices::U32(indices)) = mesh.indices() else {
            panic!("mesh has no indices");
        };
        indices
            .chunks_exact(3)
            .map(|face| {
                [
                    Vec3::from_array(positions[face[0] as usize]),
                    Vec3::from_array(positions[face[1] as usize]),
                    Vec3::from_array(positions[face[2] as usize]),
                ]
            })
            .collect()
    }

    fn positions(mesh: &Mesh) -> Vec<[f32; 3]> {
        mesh.attribute(Mesh::ATTRIBUTE_POSITION)
            .and_then(|values| values.as_float3())
            .expect("mesh has positions")
            .to_vec()
    }

    fn indices(mesh: &Mesh) -> Vec<u32> {
        let Some(Indices::U32(indices)) = mesh.indices() else {
            panic!("mesh has no indices");
        };
        indices.clone()
    }

    fn x_extent(mesh: &Mesh) -> Option<(f32, f32)> {
        positions(mesh).into_iter().fold(None, |extent, point| {
            Some(match extent {
                Some((min, max)) => (min.min(point[0]), max.max(point[0])),
                None => (point[0], point[0]),
            })
        })
    }

    fn winding_normal(face: [Vec3; 3]) -> Vec3 {
        (face[1] - face[0])
            .cross(face[2] - face[0])
            .normalize_or_zero()
    }

    #[test]
    fn width_is_monotonic() {
        assert_eq!(text_width(""), 0.0);
        assert_eq!(text_width("   "), 0.0);
        assert!(text_width("A") < text_width("AA"));
        assert!(text_width("WARD") < text_width("WARDHOUSE"));
    }

    #[test]
    fn width_matches_mesh_extent_approximately() {
        let text = "SRC-UTILS/CORE…";
        let size = 12.0;
        let stroke = 0.8;
        let mesh = text_mesh(text, size, stroke);
        let (min_x, max_x) = x_extent(&mesh).expect("text has geometry");
        let actual = max_x - min_x;
        let expected = text_width(text) * size;

        // Glyph side bearings create the visual tracking, while each stroke is
        // extended by half its width so joins do not notch. The occupied vertex
        // extent therefore differs from the logical advance by about one side
        // bearing plus the cap extension, not by an amount that grows per glyph.
        let tolerance = size * 0.12 + stroke * 2.0;
        assert!(
            (actual - expected).abs() <= tolerance,
            "extent {actual} did not match width {expected} within {tolerance}"
        );
    }

    #[test]
    fn every_visible_supported_glyph_produces_triangles() {
        for character in VISIBLE_SUPPORTED.chars() {
            let mesh = text_mesh(&character.to_string(), 10.0, 0.6);
            assert!(
                !triangles(&mesh).is_empty(),
                "{character:?} produced no triangles"
            );
        }
    }

    #[test]
    fn space_advances_without_drawing_when_part_of_text() {
        assert!(text_width("A A") > text_width("AA"));
        assert!(!triangles(&text_mesh("A A", 10.0, 0.6)).is_empty());
    }

    #[test]
    fn unknown_characters_use_placeholder_geometry() {
        let mesh = text_mesh("☃", 10.0, 0.6);
        assert!(!triangles(&mesh).is_empty());
    }

    #[test]
    fn lowercase_and_uppercase_match() {
        let lower = text_mesh("abc-xyz/123…", 10.0, 0.6);
        let upper = text_mesh("ABC-XYZ/123…", 10.0, 0.6);
        assert_eq!(positions(&lower), positions(&upper));
        assert_eq!(indices(&lower), indices(&upper));
    }

    #[test]
    fn all_triangles_face_upward() {
        let mesh = text_mesh(VISIBLE_SUPPORTED, 10.0, 0.6);
        for face in triangles(&mesh) {
            assert!(
                winding_normal(face).y > 0.99,
                "a text triangle faced {:?}",
                winding_normal(face)
            );
        }
    }

    #[test]
    fn empty_text_has_no_triangles() {
        assert!(triangles(&text_mesh("", 10.0, 0.6)).is_empty());
        assert!(triangles(&text_mesh("   ", 10.0, 0.6)).is_empty());
    }

    #[test]
    fn caps_rise_toward_negative_z() {
        // The isometric camera puts -z at the top of the screen, so a glyph's
        // cap has to sit at a smaller z than its baseline or the name reads
        // upside down on the ground.
        let mesh = text_mesh("I", 10.0, 0.6);
        let (min_z, max_z) = positions(&mesh)
            .into_iter()
            .fold((f32::INFINITY, f32::NEG_INFINITY), |(min, max), point| {
                (min.min(point[2]), max.max(point[2]))
            });
        assert!(min_z < -9.0, "cap reached only {min_z}");
        assert!(max_z <= 0.6, "baseline sat at {max_z}");
    }
}
