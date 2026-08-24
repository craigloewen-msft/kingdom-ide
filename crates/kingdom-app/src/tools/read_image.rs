//! `read_image`: looking at a picture.
//!
//! The tool that makes `browser_take_screenshot` worth calling. A screenshot
//! that a model cannot look at is a file nobody opens; this is the other half
//! of that feature, and it is deliberately general -- a diagram or a mockup the
//! user left in the workspace is just as readable as a capture.
//!
//! Unlike every other tool here, the result of this one is not words. See
//! [`kingdom_core::ToolOutcome::seen`] for why images travel beside the text
//! rather than inside it, and `llm/copilot.rs` for how they reach a model that
//! can actually see.

use super::{Refusal, Tool, Sandbox};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use kingdom_core::{ToolImage, ToolOutcome};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::Path;

/// The largest picture worth sending to a model.
///
/// Not a filesystem limit -- a limit on what is reasonable to put in a request.
/// Base64 inflates by a third, so 5 MB on disk is nearer 7 MB on the wire, which
/// is already a substantial fraction of a context window spent on one image.
const LARGEST: u64 = 5 * 1024 * 1024;

/// What we will look at, and what to call it on the wire.
///
/// Keyed by extension rather than sniffed from magic bytes: the media type has
/// to be declared to the provider anyway, and a file whose name lies about its
/// contents is a problem the model will report far more clearly than we could.
const READABLE: &[(&str, &str)] = &[
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
];

fn media_type(path: &Path) -> Option<&'static str> {
    let extension = path.extension()?.to_str()?.to_lowercase();
    READABLE
        .iter()
        .find(|(e, _)| *e == extension)
        .map(|(_, media)| *media)
}

#[derive(Deserialize)]
struct ReadImageInput {
    path: String,
}

pub struct ReadImage;

#[async_trait::async_trait]
impl Tool for ReadImage {
    fn name(&self) -> &'static str {
        "read_image"
    }

    fn description(&self) -> String {
        "Look at an image file: a screenshot, a diagram, a mockup. Required \
         after browser_take_screenshot, which saves a PNG but cannot show it to \
         you. Reads PNG, JPEG, GIF and WebP."
            .into()
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["path"],
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the image, absolute or relative to the workspace."
                }
            }
        })
    }

    async fn run(&self, input: Value, shop: &Sandbox) -> ToolOutcome {
        let input: ReadImageInput = match serde_json::from_value(input) {
            Ok(input) => input,
            Err(error) => {
                return Refusal::BadArguments {
                    tool: self.name().to_string(),
                    detail: error.to_string(),
                }
                .into()
            }
        };

        // The workspace boundary, same as every other tool that takes a path.
        let path = match shop.resolve(&input.path) {
            Ok(path) => path,
            Err(refusal) => return refusal.into(),
        };

        let Some(media) = media_type(&path) else {
            let known: Vec<_> = READABLE.iter().map(|(e, _)| *e).collect();
            return Refusal::Refused(format!(
                "{} is not an image this can read. Readable: {}.",
                input.path,
                known.join(", ")
            ))
            .into();
        };

        let metadata = match tokio::fs::metadata(&path).await {
            Ok(metadata) => metadata,
            Err(error) => {
                return Refusal::Refused(format!("Could not read {}: {error}", input.path)).into()
            }
        };

        if !metadata.is_file() {
            return Refusal::Refused(format!("{} is not a file.", input.path)).into();
        }

        if metadata.len() > LARGEST {
            return Refusal::Refused(format!(
                "{} is {} bytes, larger than the {LARGEST}-byte limit on an image.",
                input.path,
                metadata.len()
            ))
            .into();
        }

        let bytes = match tokio::fs::read(&path).await {
            Ok(bytes) => bytes,
            Err(error) => {
                return Refusal::Refused(format!("Could not read {}: {error}", input.path)).into()
            }
        };

        // The text is not a duplicate of the picture -- it is what the
        // conversation renders, what the plan's record keeps, and what a model
        // without vision is left with. The bytes ride the separate channel.
        ToolOutcome::seen(
            format!("Looked at {} ({} bytes).", path.display(), bytes.len()),
            vec![ToolImage {
                media_type: media.to_string(),
                data: BASE64.encode(&bytes),
            }],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kingdom_core::Workspace;

    /// A 1x1 transparent PNG. Small enough to inline, and a real one -- a test
    /// that reads bytes should read bytes something could actually decode.
    const A_REAL_PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    /// The whole point of the tool: bytes arrive on the image channel, and the
    /// text stays human-sized. If the payload ever leaks into `output` it would
    /// work in a test that only checked success, while making the conversation
    /// unreadable and the prompt enormous -- so both halves are asserted.
    #[tokio::test]
    async fn a_picture_travels_beside_the_words_not_inside_them() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("shot.png"), A_REAL_PNG).unwrap();
        let shop = Sandbox::new(Workspace::in_place(dir.path().to_string_lossy()));

        let outcome = ReadImage.run(json!({ "path": "shot.png" }), &shop).await;

        let ToolOutcome::Done { output, images } = outcome else {
            panic!("reading a real png should succeed: {outcome:?}");
        };
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].media_type, "image/png");
        assert_eq!(
            BASE64.decode(&images[0].data).unwrap(),
            A_REAL_PNG,
            "what comes back must be the file that went in"
        );
        assert!(
            !output.contains(&images[0].data),
            "the payload must not be duplicated into the text: {output}"
        );
    }

    /// A path outside the workspace is refused here exactly as it is everywhere
    /// else. Worth pinning for this tool specifically because the obvious port
    /// from Phoenix resolves paths itself and would quietly skip the boundary.
    #[tokio::test]
    async fn a_picture_outside_the_workspace_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let shop = Sandbox::new(Workspace::in_place(dir.path().to_string_lossy()));

        let outcome = ReadImage
            .run(json!({ "path": "../elsewhere.png" }), &shop)
            .await;

        assert!(
            matches!(outcome, ToolOutcome::Refused { .. }),
            "a path leaving the workspace must be refused: {outcome:?}"
        );
    }

    /// Refusing by extension keeps an unreadable file from being sent to a
    /// provider as though it were a picture, which costs a turn and returns an
    /// opaque gateway error rather than something the model can act on.
    #[tokio::test]
    async fn something_that_is_not_an_image_is_refused_with_the_list() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"not a picture").unwrap();
        let shop = Sandbox::new(Workspace::in_place(dir.path().to_string_lossy()));

        let outcome = ReadImage.run(json!({ "path": "notes.txt" }), &shop).await;

        let ToolOutcome::Refused { reason } = outcome else {
            panic!("a text file is not readable as an image: {outcome:?}");
        };
        assert!(
            reason.contains("png"),
            "the refusal should say what it can read: {reason}"
        );
    }
}
