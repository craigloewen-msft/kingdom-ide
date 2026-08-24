//! Editing a file, by naming the text to change rather than the line to change.
//!
//! The court's hands. Line numbers were the obvious alternative and are the
//! wrong one: a model that read a file two deeds ago has stale numbers, and an
//! edit aimed at line 412 lands wherever line 412 happens to be now. Anchoring
//! on the text itself makes a stale view *fail* instead of silently corrupting
//! a file, which is the only failure mode worth having here.
//!
//! Three rules carry that promise, and each is a refusal rather than a guess:
//!
//! - An anchor that matches more than once is refused. Picking the first of
//!   three identical sites is right one time in three and undetectable the
//!   other two.
//! - Every patch in one call is resolved against the *original* file, all at
//!   once. Resolving them in sequence would mean patch two searching text patch
//!   one already rewrote -- so an anchor that was unique when the model wrote
//!   the call could vanish, or worse, become ambiguous, halfway through.
//! - Overlapping edits are refused for the same reason: two edits over one span
//!   have no defined result, and inventing an order would make the tool's
//!   behaviour depend on argument order in a way no model could predict.
//!
//! The write itself goes through a temp file and a rename. A source file caught
//! half-written is worse than one left untouched: the plan's next build fails
//! for a reason unrelated to anything the court believes it did.

use super::{Refusal, Tool, Sandbox};
use kingdom_core::ToolOutcome;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;
use std::sync::Mutex;

/// How much diff one deed reports.
///
/// The diff is pasted into the next request to the model, so an overwrite of a
/// large file would otherwise spend the context window restating a file the
/// model just wrote. Truncation is announced, because a silently short diff
/// reads as "only this much changed".
const MAX_DIFF_LINES: usize = 200;

/// Above this, a failed anchor gets no near-miss hunt.
///
/// The hunt is O(lines x anchor) character diffing and only runs on the failure
/// path, but a generated file of a million lines would still stall the turn.
const MAX_DIAGNOSTIC_BYTES: usize = 1024 * 1024;

/// How alike a line must be to be worth showing as "did you mean this".
///
/// Tuned to catch a re-typed line -- a changed word, lost indentation, a
/// smartened quote -- and to stay quiet otherwise. A near-miss that is not
/// actually near is worse than none: it sends the model to edit the wrong site.
const NEAR_MISS_THRESHOLD: f32 = 0.6;

/// Named clipboards, per plan, outliving a single deed.
///
/// A move is two deeds -- cut here, paste there -- and the whole point of the
/// clipboard is that the text crossing between them is *the file's own bytes*,
/// never retyped by the model. That requires state between calls, and a tool is
/// a fresh value on every call by [`super::all`], so the state cannot live in
/// `Patch`.
///
/// Keyed by plan so two plans working at once cannot read each other's
/// clipboard -- which would be a cross-workspace content leak through a tool
/// whose paths are otherwise carefully bounded.
static CLIPBOARDS: Mutex<Option<HashMap<String, HashMap<String, String>>>> = Mutex::new(None);

pub struct Patch;

#[derive(Debug, Deserialize)]
struct Input {
    path: String,
    patches: Vec<Request>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Request {
    operation: Operation,
    old_text: Option<String>,
    new_text: Option<String>,
    #[serde(default)]
    replace_all: bool,
    to_clipboard: Option<String>,
    from_clipboard: Option<String>,
    reindent: Option<Reindent>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reindent {
    strip: Option<String>,
    add: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Operation {
    Replace,
    InsertBefore,
    InsertAfter,
    AppendEof,
    PrependBof,
    Overwrite,
}

impl Operation {
    fn needs_anchor(self) -> bool {
        matches!(
            self,
            Operation::Replace | Operation::InsertBefore | Operation::InsertAfter
        )
    }

    fn name(self) -> &'static str {
        match self {
            Operation::Replace => "replace",
            Operation::InsertBefore => "insert_before",
            Operation::InsertAfter => "insert_after",
            Operation::AppendEof => "append_eof",
            Operation::PrependBof => "prepend_bof",
            Operation::Overwrite => "overwrite",
        }
    }
}

/// One resolved edit: a byte span of the original and what replaces it.
///
/// Byte spans rather than line numbers because every edit here is resolved
/// against one immutable original, and spans are what makes "do these two
/// overlap?" a comparison instead of a re-search.
#[derive(Debug, Clone)]
struct Edit {
    offset: usize,
    length: usize,
    replacement: String,
}

#[async_trait::async_trait]
impl Tool for Patch {
    fn name(&self) -> &'static str {
        "patch"
    }

    fn description(&self) -> String {
        r#"Edit a file by naming the text to change, not the line number.

Operations:
- replace: substitute `oldText` with `newText`
- insert_before / insert_after: place `newText` beside the `oldText` anchor, leaving the anchor itself untouched
- append_eof / prepend_bof: add `newText` at the end or the start of the file
- overwrite: replace the whole file (creates it if it does not exist)

`oldText` must appear EXACTLY ONCE. If it does not, widen it with surrounding \
lines until it does -- an ambiguous anchor is refused, never guessed at. Set \
`replaceAll` (replace only) to change every exact occurrence deliberately.

All patches in one call are resolved against the ORIGINAL file at once, not one \
after another. So a later patch cannot anchor on text an earlier one wrote, and \
two patches covering the same span are refused. For edits that must build on \
each other, make separate calls.

Clipboards (`toClipboard` / `fromClipboard`) carry the file's own bytes between \
patches and between calls. Always use them to move or copy code rather than \
retyping it: retyped code is where silent transcription errors come from.
- cut: replace with empty `newText` and `toClipboard`
- paste: any operation with `fromClipboard`

`reindent` adjusts whatever is being inserted: `strip` comes off the front of \
each non-empty line, then `add` goes on. Use it when moved code changes nesting.

Everything is literal: no newline is added or trimmed for you."#
            .to_string()
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["path", "patches"],
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File to edit, relative to the workspace root."
                },
                "patches": {
                    "type": "array",
                    "description": "Edits to apply, all resolved against the original file.",
                    "items": {
                        "type": "object",
                        "required": ["operation"],
                        "properties": {
                            "operation": {
                                "type": "string",
                                "enum": ["replace", "insert_before", "insert_after",
                                         "append_eof", "prepend_bof", "overwrite"]
                            },
                            "oldText": {
                                "type": "string",
                                "description": "Text to locate. Required for replace, \
                                                insert_before and insert_after. Must occur \
                                                exactly once unless replaceAll is set."
                            },
                            "newText": {
                                "type": "string",
                                "description": "Text to write. Empty deletes. Ignored when \
                                                fromClipboard is set."
                            },
                            "replaceAll": {
                                "type": "boolean",
                                "description": "replace only: change every exact occurrence."
                            },
                            "toClipboard": {
                                "type": "string",
                                "description": "Store the matched text under this name."
                            },
                            "fromClipboard": {
                                "type": "string",
                                "description": "Use this clipboard's contents as newText."
                            },
                            "reindent": {
                                "type": "object",
                                "description": "Reindent the inserted text before it lands.",
                                "properties": {
                                    "strip": {"type": "string"},
                                    "add": {"type": "string"}
                                }
                            }
                        }
                    }
                }
            }
        })
    }

    async fn run(&self, input: Value, shop: &Sandbox) -> ToolOutcome {
        let input: Input = match serde_json::from_value(input) {
            Ok(i) => i,
            Err(e) => {
                return Refusal::BadArguments {
                    tool: "patch".to_string(),
                    detail: e.to_string(),
                }
                .into()
            }
        };

        if input.patches.is_empty() {
            return Refusal::BadArguments {
                tool: "patch".to_string(),
                detail: "`patches` was empty; give at least one edit".to_string(),
            }
            .into();
        }

        let resolved = match shop.resolve(&input.path) {
            Ok(p) => p,
            Err(refusal) => return refusal.into(),
        };

        let original = match std::fs::read_to_string(&resolved) {
            Ok(text) => Some(text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                return Refusal::Refused(format!(
                    "{} could not be read: {e}. Nothing was written.",
                    input.path
                ))
                .into()
            }
        };

        // Clipboards are taken for the whole plan and put back only once the
        // write has landed. A call that refuses must leave none of its own
        // `toClipboard` writes behind, or the next `fromClipboard` pastes text
        // from an edit that never happened.
        let mut clipboards = clipboards_for(shop.plan().as_str());
        let planned = plan(&input.path, original.as_deref(), &input.patches, &mut clipboards);

        let (updated, diff) = match planned {
            Ok(pair) => pair,
            Err(reason) => return Refusal::Refused(reason).into(),
        };

        if let Err(e) = write_atomically(&resolved, &updated) {
            return Refusal::Refused(format!(
                "{} could not be written: {e}. The file is unchanged.",
                input.path
            ))
            .into();
        }

        store_clipboards(shop.plan().as_str(), clipboards);

        ToolOutcome::done(format!("Patched {}.\n\n{}", input.path, bounded(&diff)))
    }
}

/// Resolves every patch against `original` and returns the new content and a diff.
///
/// Split out from [`Tool::run`] so the semantics that matter -- uniqueness,
/// simultaneity, overlap -- are testable without a filesystem, and so the only
/// code that can leave a file changed is the handful of lines above.
fn plan(
    path: &str,
    original: Option<&str>,
    patches: &[Request],
    clipboards: &mut HashMap<String, String>,
) -> Result<(String, String), String> {
    if original.is_none() {
        if let Some(bad) = patches.iter().find(|p| p.operation.needs_anchor()) {
            return Err(format!(
                "{path} does not exist, so `{}` has no text to anchor on. \
                 Use `overwrite` to create the file.",
                bad.operation.name()
            ));
        }
    }
    let original = original.unwrap_or_default();

    let mut edits: Vec<Edit> = Vec::new();

    for (i, patch) in patches.iter().enumerate() {
        let number = i + 1;

        if patch.replace_all && patch.operation != Operation::Replace {
            return Err(format!(
                "patch {number}: `replaceAll` only means anything for `replace`, \
                 not `{}`. Drop the flag, or use `replace`.",
                patch.operation.name()
            ));
        }

        // An empty clipboard name is treated as absent: models pass `""` where
        // they mean "none", and refusing there costs a turn to learn nothing.
        let replacement = match patch.from_clipboard.as_deref().filter(|n| !n.is_empty()) {
            Some(name) => clipboards.get(name).cloned().ok_or_else(|| {
                format!(
                    "patch {number}: nothing has been stored in clipboard `{name}`. \
                     Store it first with `toClipboard` on the patch that cuts it."
                )
            })?,
            None => patch.new_text.clone().unwrap_or_default(),
        };
        let replacement = match &patch.reindent {
            Some(r) => reindent(&replacement, r).map_err(|e| format!("patch {number}: {e}"))?,
            None => replacement,
        };

        let spans = match patch.operation {
            Operation::PrependBof => vec![(0, 0)],
            Operation::AppendEof => vec![(original.len(), 0)],
            Operation::Overwrite => vec![(0, original.len())],
            _ => {
                let anchor = patch.old_text.as_deref().ok_or_else(|| {
                    format!(
                        "patch {number}: `{}` needs `oldText` to anchor on.",
                        patch.operation.name()
                    )
                })?;

                if patch.replace_all {
                    let all = exact_matches(original, anchor);
                    if all.is_empty() {
                        return Err(not_found(path, number, original, anchor));
                    }
                    all
                } else {
                    let (offset, length) = locate(original, anchor)
                        .map_err(|e| e.explain(path, number, original, anchor))?;
                    match patch.operation {
                        Operation::InsertBefore => vec![(offset, 0)],
                        Operation::InsertAfter => vec![(offset + length, 0)],
                        _ => vec![(offset, length)],
                    }
                }
            }
        };

        // The file's own bytes, not the model's `oldText` -- when a fuzzy match
        // recovered a near-miss those differ, and it is the file that is about
        // to be moved somewhere else.
        if let Some(name) = patch.to_clipboard.as_deref().filter(|n| !n.is_empty()) {
            if let Some(&(offset, length)) = spans.first() {
                if length > 0 {
                    clipboards.insert(name.to_string(), original[offset..offset + length].to_string());
                }
            }
        }

        for (offset, length) in spans {
            edits.push(Edit {
                offset,
                length,
                replacement: replacement.clone(),
            });
        }
    }

    let updated = apply(original, edits)?;
    let diff = unified_diff(path, original, &updated);
    Ok((updated, diff))
}

/// Splices every edit in, working from the end of the file backwards.
///
/// Backwards so that an offset computed against the original is still correct
/// when its turn comes; forwards would need every later offset shifted by the
/// length delta of every earlier edit, which is the arithmetic that goes wrong.
fn apply(original: &str, edits: Vec<Edit>) -> Result<String, String> {
    let mut indexed: Vec<(usize, Edit)> = edits.into_iter().enumerate().collect();

    // Ties at one offset: longer spans first, then later-requested first. The
    // second rule is what makes two `insert_before` on the same anchor come out
    // in the order they were asked for, since each is spliced ahead of the last.
    indexed.sort_by_key(|(i, e)| {
        (
            std::cmp::Reverse(e.offset),
            std::cmp::Reverse(e.length),
            std::cmp::Reverse(*i),
        )
    });

    let mut result = original.to_string();
    let mut previous_start: Option<usize> = None;

    for (_, edit) in &indexed {
        let end = edit.offset + edit.length;
        if let Some(start) = previous_start {
            // Touching at a boundary is fine -- an insert at the exact start of
            // a replaced span has an unambiguous result. Anything past it does
            // not, so it is refused rather than resolved by argument order.
            if end > start {
                return Err(format!(
                    "two of these patches cover the same text (bytes {}..{} and {start}..). \
                     Only one edit may touch a given span in one call; make the second a \
                     separate call, or widen the first to cover both changes.",
                    edit.offset, end
                ));
            }
        }
        previous_start = Some(edit.offset);
        result.replace_range(edit.offset..end, &edit.replacement);
    }

    Ok(result)
}

/// Every exact, non-overlapping occurrence of `anchor`.
///
/// Used only by `replaceAll`, which deliberately does not consult the fuzzy
/// cascade below: "the single best candidate" has no meaning across many sites,
/// and a near-miss silently joining a bulk rename is exactly the kind of edit
/// nobody reviews.
fn exact_matches(content: &str, anchor: &str) -> Vec<(usize, usize)> {
    if anchor.is_empty() {
        return Vec::new();
    }
    content
        .match_indices(anchor)
        .map(|(offset, _)| (offset, anchor.len()))
        .collect()
}

enum MatchFailure {
    NotFound,
    Ambiguous(Vec<usize>),
}

impl MatchFailure {
    fn explain(self, path: &str, number: usize, content: &str, anchor: &str) -> String {
        match self {
            MatchFailure::NotFound => not_found(path, number, content, anchor),
            MatchFailure::Ambiguous(offsets) => {
                let lines: Vec<String> = offsets
                    .iter()
                    .take(5)
                    .map(|&o| format!("line {}", line_of(content, o)))
                    .collect();
                format!(
                    "patch {number}: that `oldText` appears {} times in {path} ({}). \
                     Refusing rather than guessing which one you meant -- an edit at the \
                     wrong one of these is silent corruption. Add surrounding lines until \
                     the anchor is unique, or set `replaceAll` if you mean all of them.",
                    offsets.len(),
                    lines.join(", ")
                )
            }
        }
    }
}

/// Finds the one place `anchor` belongs, or says why it cannot.
///
/// Exact first, then a whitespace-tolerant pass over whole lines. The tolerant
/// pass exists because the overwhelmingly common near-miss is indentation: a
/// model quoting a block it read at one nesting level and pasting it back at
/// another. Refusing that costs a turn and produces an identical retry. What it
/// deliberately will *not* do is tolerate a difference in the non-whitespace
/// text -- that is a model misremembering the code, and it must be told.
fn locate(content: &str, anchor: &str) -> Result<(usize, usize), MatchFailure> {
    let exact = exact_matches(content, anchor);
    match exact.len() {
        1 => return Ok(exact[0]),
        0 => {}
        _ => return Err(MatchFailure::Ambiguous(exact.into_iter().map(|(o, _)| o).collect())),
    }

    let loose = whitespace_insensitive_matches(content, anchor);
    match loose.len() {
        1 => Ok(loose[0]),
        0 => Err(MatchFailure::NotFound),
        _ => Err(MatchFailure::Ambiguous(
            loose.into_iter().map(|(o, _)| o).collect(),
        )),
    }
}

/// Matches the anchor line by line, ignoring each line's leading and trailing
/// whitespace but nothing else.
fn whitespace_insensitive_matches(content: &str, anchor: &str) -> Vec<(usize, usize)> {
    let anchor_lines: Vec<&str> = anchor.split_inclusive('\n').collect();
    if anchor_lines.is_empty() {
        return Vec::new();
    }

    let mut offset = 0usize;
    let content_lines: Vec<(usize, &str)> = content
        .split_inclusive('\n')
        .map(|line| {
            let at = offset;
            offset += line.len();
            (at, line)
        })
        .collect();

    let mut found = Vec::new();
    let last = anchor_lines.len() - 1;

    for start in 0..content_lines.len().saturating_sub(last) {
        let window = &content_lines[start..=start + last];
        if !window
            .iter()
            .zip(&anchor_lines)
            .all(|((_, c), a)| c.trim() == a.trim())
        {
            continue;
        }

        let (first_at, _) = window[0];
        let (last_at, last_line) = window[last];
        // An anchor whose final line has no newline must not swallow the file's:
        // consuming it would delete the line break between the replaced text and
        // whatever follows.
        let tail = if anchor_lines[last].ends_with('\n') {
            last_line.len()
        } else {
            last_line.trim_end_matches('\n').trim_end().len()
        };
        found.push((first_at, last_at + tail - first_at));
    }

    found
}

/// A refusal for an anchor that is nowhere in the file, carrying the closest
/// thing that is.
///
/// A bare "not found" is the worst possible answer: it gives the model nothing
/// to change, so it retries the identical call and the turn is spent. Shown the
/// line it *nearly* matched, it can see the word it misremembered.
fn not_found(path: &str, number: usize, content: &str, anchor: &str) -> String {
    let mut reason = format!(
        "patch {number}: that `oldText` is not in {path} -- not exactly, and not \
         ignoring indentation. Nothing was written."
    );

    if let Some((line, text)) = near_miss(content, anchor) {
        let _ = write!(
            reason,
            "\n\nThe closest thing in the file is line {line}:\n  {text}\n\n\
             If that is the place you meant, copy it exactly -- including \
             whitespace -- and try again. Otherwise re-read the file: it may \
             have changed since you last saw it."
        );
    } else {
        reason.push_str(
            " Nothing in the file resembles it; re-read the file rather than \
             retrying this anchor.",
        );
    }

    reason
}

/// The file line most like the anchor's first distinctive line.
fn near_miss(content: &str, anchor: &str) -> Option<(usize, String)> {
    if content.len() > MAX_DIAGNOSTIC_BYTES {
        return None;
    }

    let needle = anchor.lines().map(str::trim).find(|l| l.len() >= 4)?;

    let (index, line, score) = content
        .lines()
        .enumerate()
        .map(|(i, line)| {
            let score = similar::TextDiff::from_chars(needle, line.trim()).ratio();
            (i, line, score)
        })
        .max_by(|a, b| a.2.total_cmp(&b.2))?;

    (score >= NEAR_MISS_THRESHOLD).then(|| (index + 1, line.trim_end().to_string()))
}

fn line_of(content: &str, offset: usize) -> usize {
    content[..offset].matches('\n').count() + 1
}

/// Restrips and repads each non-empty line of text about to be inserted.
///
/// A line that does not start with `strip` is a refusal rather than a
/// pass-through: silently leaving one line at the old indentation inside an
/// otherwise-moved block produces code that compiles and reads wrong.
fn reindent(text: &str, spec: &Reindent) -> Result<String, String> {
    let mut out = Vec::new();

    for line in text.split('\n') {
        if line.is_empty() {
            out.push(String::new());
            continue;
        }

        let stripped = match &spec.strip {
            Some(prefix) => line.strip_prefix(prefix.as_str()).ok_or_else(|| {
                format!("this line does not start with the `strip` prefix {prefix:?}: {line:?}")
            })?,
            None => line,
        };

        out.push(match &spec.add {
            Some(prefix) => format!("{prefix}{stripped}"),
            None => stripped.to_string(),
        });
    }

    Ok(out.join("\n"))
}

fn unified_diff(path: &str, old: &str, new: &str) -> String {
    similar::TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(3)
        .header(&format!("a/{path}"), &format!("b/{path}"))
        .to_string()
}

fn bounded(diff: &str) -> String {
    let total = diff.lines().count();
    if total <= MAX_DIFF_LINES {
        return diff.to_string();
    }

    let mut out: String = diff
        .lines()
        .take(MAX_DIFF_LINES)
        .map(|l| format!("{l}\n"))
        .collect();
    let _ = writeln!(
        out,
        "[{} more diff lines not shown. The whole patch was applied; read the \
         file if you need to see the rest.]",
        total - MAX_DIFF_LINES
    );
    out
}

/// Writes beside the target and renames over it.
///
/// The house pattern, from `store.rs::save`, and it matters more here: a plan's
/// records can be rebuilt, but a source file truncated mid-write fails the next
/// build for a reason that has nothing to do with the edit the court believes
/// it made. Rename within a directory is atomic, so the file is either the old
/// one or the new one.
fn write_atomically(path: &Path, content: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "patch".to_string());
    let temp = path.with_file_name(format!(".{name}.kingdom-tmp"));

    std::fs::write(&temp, content)?;
    std::fs::rename(&temp, path)
}

fn clipboards_for(plan: &str) -> HashMap<String, String> {
    let guard = CLIPBOARDS.lock().unwrap_or_else(|e| e.into_inner());
    guard
        .as_ref()
        .and_then(|all| all.get(plan))
        .cloned()
        .unwrap_or_default()
}

fn store_clipboards(plan: &str, clipboards: HashMap<String, String>) {
    let mut guard = CLIPBOARDS.lock().unwrap_or_else(|e| e.into_inner());
    guard
        .get_or_insert_with(HashMap::new)
        .insert(plan.to_string(), clipboards);
}

#[cfg(test)]
mod tests {
    use super::*;
    use kingdom_core::Workspace;

    fn requests(value: Value) -> Vec<Request> {
        serde_json::from_value(value).expect("test patch requests must parse")
    }

    fn planned(original: &str, patches: Value) -> Result<String, String> {
        let mut clipboards = HashMap::new();
        plan(
            "f.rs",
            Some(original),
            &requests(patches),
            &mut clipboards,
        )
        .map(|(updated, _)| updated)
    }

    /// The rule the whole tool rests on. Three identical call sites is the
    /// ordinary shape of real code, and picking the first is right by accident
    /// and wrong invisibly -- the file still compiles, the change is simply in
    /// the wrong function.
    #[test]
    fn an_anchor_matching_more_than_once_is_refused_not_guessed() {
        let reason = planned(
            "call();\ncall();\ncall();\n",
            json!([{"operation": "replace", "oldText": "call();", "newText": "gone();"}]),
        )
        .expect_err("an ambiguous anchor must be refused");

        assert!(reason.contains("3 times"), "{reason}");
        assert!(
            reason.contains("line 1") && reason.contains("line 3"),
            "the model needs to see where the rival sites are: {reason}"
        );
    }

    /// `replaceAll` is the deliberate way to say "all three", and it must not
    /// need the anchor to be unique -- that is the entire point of the flag.
    #[test]
    fn replace_all_changes_every_occurrence() {
        let out = planned(
            "call();\ncall();\n",
            json!([{"operation": "replace", "oldText": "call();",
                    "newText": "gone();", "replaceAll": true}]),
        )
        .unwrap();

        assert_eq!(out, "gone();\ngone();\n");
    }

    /// Simultaneous resolution. Both anchors are unique in the *original*, and
    /// the second must still be found after the first is planned -- if patches
    /// resolved in sequence, an anchor spanning text the first patch rewrote
    /// would vanish partway through a call the model had every reason to expect
    /// would work.
    #[test]
    fn every_patch_resolves_against_the_original_file() {
        let out = planned(
            "one\ntwo\nthree\n",
            json!([
                {"operation": "replace", "oldText": "one", "newText": "ONE"},
                {"operation": "replace", "oldText": "three", "newText": "THREE"}
            ]),
        )
        .unwrap();

        assert_eq!(out, "ONE\ntwo\nTHREE\n");
    }

    /// The other half of simultaneity: two edits over one span have no defined
    /// result, so the call is refused whole rather than resolved by argument
    /// order -- which no model could predict and no reviewer would notice.
    #[test]
    fn two_patches_over_the_same_span_are_refused() {
        let reason = planned(
            "let x = compute(1);\n",
            json!([
                {"operation": "replace", "oldText": "compute(1)", "newText": "compute(2)"},
                {"operation": "replace", "oldText": "= compute", "newText": "= recompute"}
            ]),
        )
        .expect_err("overlapping edits must be refused");

        assert!(reason.contains("same text"), "{reason}");
    }

    /// A move is the clipboard's reason to exist: the bytes that land at the
    /// destination must be the file's own, byte for byte, because the failure
    /// this prevents is a model retyping a block and changing one character of
    /// it on the way past.
    #[test]
    fn a_clipboard_move_carries_the_files_own_bytes() {
        let mut clipboards = HashMap::new();
        let original = "fn a() {\n    keep();\n}\n\nfn b() {\n}\n";

        let (cut, _) = plan(
            "f.rs",
            Some(original),
            &requests(json!([{
                "operation": "replace",
                "oldText": "    keep();\n",
                "newText": "",
                "toClipboard": "body"
            }])),
            &mut clipboards,
        )
        .unwrap();

        let (pasted, _) = plan(
            "f.rs",
            Some(&cut),
            &requests(json!([{
                "operation": "insert_after",
                "oldText": "fn b() {\n",
                "fromClipboard": "body"
            }])),
            &mut clipboards,
        )
        .unwrap();

        assert_eq!(pasted, "fn a() {\n}\n\nfn b() {\n    keep();\n}\n");
    }

    /// A near-miss must come back with the line it nearly matched. A bare "not
    /// found" tells the model nothing to change, so it retries the identical
    /// call and the turn is spent for nothing.
    #[test]
    fn a_missing_anchor_names_the_closest_line_in_the_file() {
        let reason = planned(
            "fn compute(value: usize) -> usize {\n    value + 1\n}\n",
            json!([{"operation": "replace",
                    "oldText": "fn compute(value: u32) -> u32 {",
                    "newText": "x"}]),
        )
        .expect_err("a mistyped anchor must be refused");

        assert!(
            reason.contains("line 1") && reason.contains("value: usize"),
            "the refusal must show the near-miss: {reason}"
        );
    }

    /// Indentation is the near-miss worth *recovering* from rather than
    /// reporting: a model quoting a block it read nested one level deeper is
    /// unambiguously pointing at one place, and refusing produces an identical
    /// retry. Anything beyond whitespace is a misremembering and still refused,
    /// which the test above pins.
    #[test]
    fn an_anchor_whose_only_error_is_indentation_still_matches() {
        let out = planned(
            "impl T {\n    fn go(&self) {\n        work();\n    }\n}\n",
            json!([{"operation": "replace",
                    "oldText": "fn go(&self) {\nwork();\n}",
                    "newText": "    fn go(&self) {}"}]),
        )
        .unwrap();

        assert_eq!(out, "impl T {\n    fn go(&self) {}\n}\n");
    }

    /// The boundary, exercised through the tool: the check is only worth
    /// anything if `run` actually routes its path through it, and a patch tool
    /// that writes outside the workspace is the worst bug this codebase could
    /// have.
    #[tokio::test]
    async fn a_path_outside_the_workspace_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let victim = outside.path().join("victim.txt");
        std::fs::write(&victim, "untouched\n").unwrap();

        let shop = Sandbox::new(Workspace::in_place(dir.path().to_str().unwrap()));
        let outcome = Patch
            .run(
                json!({
                    "path": victim.to_str().unwrap(),
                    "patches": [{"operation": "overwrite", "newText": "owned\n"}]
                }),
                &shop,
            )
            .await;

        assert!(matches!(outcome, ToolOutcome::Refused { .. }), "{outcome:?}");
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "untouched\n");
    }

    /// The end to end path: the file on disk actually changes, and the deed
    /// reports the diff so the King and the model can both see where the edit
    /// landed without re-reading the file.
    #[tokio::test]
    async fn an_applied_patch_writes_the_file_and_reports_its_diff() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.rs"), "fn main() {}\n").unwrap();

        let shop = Sandbox::new(Workspace::in_place(dir.path().to_str().unwrap()));
        let outcome = Patch
            .run(
                json!({
                    "path": "f.rs",
                    "patches": [{"operation": "append_eof", "newText": "// end\n"}]
                }),
                &shop,
            )
            .await;

        match outcome {
            ToolOutcome::Done { output, .. } => assert!(output.contains("+// end"), "{output}"),
            ToolOutcome::Refused { reason } => panic!("refused: {reason}"),
        }
        assert_eq!(
            std::fs::read_to_string(dir.path().join("f.rs")).unwrap(),
            "fn main() {}\n// end\n"
        );
    }
}
