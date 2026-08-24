//! Reading a file, with line numbers.
//!
//! The court's cheapest way to look at something. It exists as a tool of its
//! own rather than as `cat` through a shell because the line numbers are the
//! point: a model that has read a file numbered can ask for a window of it
//! later, and can name a line to the King. `cat` gives it a wall of text with
//! no coordinates, and the next request re-reads the whole file to find one
//! function.
//!
//! Every path arrives through [`Workshop::resolve`]. Phoenix, where this is
//! ported from, deliberately dropped that check so its agent could read stdlib
//! sources and toolchain caches -- reasonable there, wrong here: a plan owns a
//! worktree precisely so that what it touches is knowable, and a reader that
//! wanders into another city's checkout makes the boundary a fiction.

use super::{Refusal, Tool, Workshop};
use kingdom_core::DeedOutcome;
use serde_json::{json, Value};
use std::fmt::Write as _;

/// How many lines one call returns unasked.
///
/// The result is pasted into the next request to the model, so this is a token
/// budget, not a comfort setting. Two thousand lines covers most source files
/// whole; beyond that the model is told what it did not see and can ask for the
/// rest by offset, which costs one turn instead of a truncated context.
const DEFAULT_LIMIT: usize = 2000;

/// How far into a file we look for a NUL before calling it binary.
///
/// A prefix rather than the whole file: the sniff runs on every read, and text
/// files do not hide their first NUL eight kilobytes in.
const BINARY_SNIFF_BYTES: usize = 8192;

pub struct ReadFile;

#[async_trait::async_trait]
impl Tool for ReadFile {
    fn name(&self) -> &'static str {
        "read_file"
    }

    fn description(&self) -> String {
        "Read a file's contents. Returns numbered lines. Use `offset` and \
         `limit` to read a window of a large file rather than re-reading the \
         whole of it."
            .to_string()
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["path"],
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File to read, relative to the workspace root."
                },
                "offset": {
                    "type": "integer",
                    "description": "Line to start from, 1-based. Default: 1."
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of lines to return. Default: 2000."
                }
            }
        })
    }

    async fn run(&self, input: Value, shop: &Workshop) -> DeedOutcome {
        let Some(path) = input.get("path").and_then(Value::as_str) else {
            return Refusal::BadArguments {
                tool: "read_file".to_string(),
                detail: "no `path` was given".to_string(),
            }
            .into();
        };

        let resolved = match shop.resolve(path) {
            Ok(p) => p,
            Err(refusal) => return refusal.into(),
        };

        // `std::fs` on the blocking pool rather than `tokio::fs`, because the
        // server's tokio is built without the `fs` feature; adding it would buy
        // nothing here, as a whole-file read is one blocking call either way.
        let read = tokio::task::spawn_blocking(move || std::fs::read(&resolved)).await;

        let bytes = match read {
            Ok(Ok(bytes)) => bytes,
            Ok(Err(e)) => {
                return Refusal::Refused(format!(
                    "{path} could not be read: {e}. Check the path, or use `search` to find it."
                ))
                .into()
            }
            Err(e) => return Refusal::Refused(format!("reading {path} did not finish: {e}")).into(),
        };

        let sniff = bytes.len().min(BINARY_SNIFF_BYTES);
        if bytes[..sniff].contains(&0) {
            return Refusal::Refused(format!(
                "{path} is a binary file and has no lines to show."
            ))
            .into();
        }

        let Ok(text) = String::from_utf8(bytes) else {
            return Refusal::Refused(format!("{path} is not valid UTF-8 text.")).into();
        };

        let offset = input
            .get("offset")
            .and_then(Value::as_u64)
            .unwrap_or(1)
            .max(1) as usize;
        let limit = input
            .get("limit")
            .and_then(Value::as_u64)
            .map_or(DEFAULT_LIMIT, |n| n as usize);

        DeedOutcome::done(window(&text, offset, limit))
    }
}

/// Numbers the requested window and says what was left out.
///
/// The trailing note is the load-bearing half. A model handed a silently
/// truncated file believes it has seen the whole of it and reasons about code
/// that is still below the cut; told how many lines remain, it asks for them.
fn window(text: &str, offset: usize, limit: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();

    // Clamped rather than refused: an offset past the end is what a model does
    // when it is paging through a file and has reached the last page, and
    // refusing there would make it retry the same walk from the top.
    let start = offset.saturating_sub(1).min(total);
    let end = start.saturating_add(limit).min(total);

    let mut out = String::new();
    for (i, line) in lines[start..end].iter().enumerate() {
        let _ = writeln!(out, "{:>6}\t{line}", start + i + 1);
    }

    let remaining = total.saturating_sub(end);
    if remaining > 0 {
        let _ = write!(
            out,
            "\n[{remaining} more lines not shown (total: {total}). \
             Call again with offset {} to continue.]",
            end + 1
        );
    }

    if out.is_empty() {
        // Distinguished from a failure on purpose: an empty file is a fact the
        // model needs, and a blank result reads as a broken tool.
        return if total == 0 {
            "(empty file)".to_string()
        } else {
            format!("(no lines at offset {offset}; the file has {total} lines)")
        };
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use kingdom_core::Workspace;
    use std::path::Path;

    fn shop(root: &Path) -> Workshop {
        Workshop::new(Workspace::in_place(root.to_str().unwrap()))
    }

    async fn read(root: &Path, input: Value) -> String {
        match ReadFile.run(input, &shop(root)).await {
            DeedOutcome::Done { output, .. } => output,
            DeedOutcome::Refused { reason } => panic!("refused: {reason}"),
        }
    }

    /// The window arithmetic, which is where an off-by-one costs the model the
    /// line it was actually looking for. `offset` is 1-based and inclusive,
    /// `limit` counts lines, and the numbers shown are the file's own.
    #[tokio::test]
    async fn a_window_is_numbered_with_the_files_own_line_numbers() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "a\nb\nc\nd\ne\n").unwrap();

        let out = read(dir.path(), json!({"path": "f.txt", "offset": 2, "limit": 2})).await;

        assert!(out.contains("     2\tb"), "{out}");
        assert!(out.contains("     3\tc"), "{out}");
        assert!(!out.contains("\td"), "limit must stop at two lines: {out}");
        assert!(
            out.contains("2 more lines not shown"),
            "the model must be told what it did not see: {out}"
        );
    }

    /// Paging off the end of a file is ordinary, not an error: a model reading
    /// a file in windows discovers the end by asking for one window too many.
    /// Refusing there earns a retry from line 1 and a wasted turn.
    #[tokio::test]
    async fn an_offset_past_the_end_is_answered_not_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "a\nb\n").unwrap();

        let out = read(dir.path(), json!({"path": "f.txt", "offset": 99})).await;

        assert!(out.contains("the file has 2 lines"), "{out}");
    }

    /// The boundary, exercised through the tool rather than through
    /// [`Workshop::resolve`] alone -- the check is only worth anything if the
    /// tool actually routes its path through it, and that is what regresses.
    #[tokio::test]
    async fn a_path_outside_the_workspace_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "secret\n").unwrap();

        let outcome = ReadFile
            .run(
                json!({"path": outside.path().join("secret.txt").to_str().unwrap()}),
                &shop(dir.path()),
            )
            .await;

        match outcome {
            DeedOutcome::Refused { reason } => assert!(reason.contains("outside"), "{reason}"),
            DeedOutcome::Done { output, .. } => panic!("read a file outside the workspace: {output}"),
        }
    }
}
