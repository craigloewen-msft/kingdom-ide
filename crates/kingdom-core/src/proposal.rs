//! Reading a proposal: splitting one into the parts the King annotates, and
//! reading a revision against the plan it revises.
//!
//! Both jobs are here rather than in the view because **both sides of the wire
//! need the same answer**. The browser splits a proposal to decide where a note
//! may be pinned; the server splits it again to quote the annotated part back to
//! the model. Two implementations of "where does a block begin" is exactly how
//! the King's note and the court's quote come to describe different paragraphs.
//!
//! Pure functions over strings, so it compiles to wasm along with the rest of
//! the domain -- see the crate docs for why that constraint is load-bearing.

use serde::{Deserialize, Serialize};

/// One annotatable part of a proposal: a heading, a paragraph, a list item, a
/// fence, a table, a quote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Block {
    /// Where this block starts in the body, 1-based.
    ///
    /// The anchor a [`crate::ProposalNote`] is pinned to, and what puts a note
    /// beside the right block while the card is open. Deliberately *not* the
    /// only thing a note carries -- see [`crate::ProposalNote::quote`] for why
    /// a line number alone would drift.
    pub line: usize,
    /// The block's own markdown, exactly as the court wrote it.
    pub text: String,
}

/// A proposal split into the parts the King can write against.
///
/// **Markdown blocks, not lines.** A note on "line 12" of a wrapped paragraph is
/// a note on a fragment of a sentence; a note on the paragraph is a note on a
/// thought. The King is judging an argument, so the unit he marks up has to be
/// a unit of argument.
///
/// Nesting is folded into the parent: a list item containing a sub-list is one
/// block, not three. Otherwise the deepest bullet in a plan would be its own
/// annotatable thing while the point it belongs to would not be -- and the King
/// would be offered a hundred targets on a document with a dozen ideas in it.
///
/// An empty or whitespace-only body yields nothing, which is the honest answer
/// rather than one block holding nothing.
pub fn blocks(body: &str) -> Vec<Block> {
    use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_TASKLISTS);

    let mut out: Vec<Block> = Vec::new();
    // How deep inside a block we are. A block is taken at depth 0; everything
    // between its start and its end is part of it and is not taken again.
    let mut depth = 0usize;
    // How many lists we are inside. A *top-level* list is not itself a block --
    // its items are, so the King can object to one bullet rather than to a
    // whole list of them. Counting rather than a flag is what keeps a nested
    // list from being mistaken for another top-level one.
    let mut lists = 0usize;

    for (event, range) in Parser::new_ext(body, options).into_offset_iter() {
        match &event {
            // A top-level list is transparent: it opens no block, so its items
            // land at depth 0 and are taken individually. A nested list is
            // already inside its parent item's block, and `depth` keeps it there.
            Event::Start(Tag::List(_)) if depth == 0 => lists += 1,
            Event::End(TagEnd::List(_)) if depth == 0 && lists > 0 => lists -= 1,

            Event::Start(_) => {
                if depth == 0 {
                    let text = body[range.clone()].trim_end().to_string();
                    if !text.trim().is_empty() {
                        out.push(Block {
                            line: line_of(body, range.start),
                            text,
                        });
                    }
                }
                depth += 1;
            }
            Event::End(_) => depth = depth.saturating_sub(1),

            // Text, code, breaks: always inside something that has already been
            // taken.
            _ => {}
        }
    }

    out
}

/// Which line a byte offset falls on, 1-based.
///
/// Counted rather than tracked incrementally because `into_offset_iter` yields
/// ranges in document order but the caller only asks about a handful of them --
/// a proposal is a document a person reads, not a file a machine streams.
fn line_of(body: &str, byte: usize) -> usize {
    body[..byte].bytes().filter(|b| *b == b'\n').count() + 1
}

/// What happened to one line between two versions of a plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Change {
    /// Present in both, untouched.
    Same,
    /// In the new version and not the old.
    Added,
    /// In the old version and not the new.
    Removed,
}

/// One line of a proposal read against its predecessor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffLine {
    pub change: Change,
    pub text: String,
}

/// A revised plan read against the one it revises, line by line.
///
/// **A diff of the markdown source, not of the rendered plan.** Rendered
/// markdown has no lines to mark, so diffing the output would mean marking DOM
/// nodes -- and a heading whose wording changed by one word is a wholly new
/// node, so the marking would say "this changed" without ever saying what. The
/// King reads a diff to find *what moved*; he reads the plan itself to judge it,
/// and the view offers him both.
///
/// Trailing newlines are dropped so a body that ends in one does not produce a
/// final empty line the King has to wonder about.
pub fn diff(old: &str, new: &str) -> Vec<DiffLine> {
    use similar::ChangeTag;

    similar::TextDiff::from_lines(old, new)
        .iter_all_changes()
        .map(|change| DiffLine {
            change: match change.tag() {
                ChangeTag::Equal => Change::Same,
                ChangeTag::Insert => Change::Added,
                ChangeTag::Delete => Change::Removed,
            },
            text: change.value().trim_end_matches(['\n', '\r']).to_string(),
        })
        .collect()
}

/// True when two bodies are the same document.
///
/// The question the view asks before drawing a diff at all: a revision that
/// changed nothing must say so in a sentence rather than as a wall of unchanged
/// lines the King reads looking for a difference that is not there.
pub fn unchanged(lines: &[DiffLine]) -> bool {
    lines.iter().all(|l| l.change == Change::Same)
}

/// How many untouched lines may sit together before they are worth collapsing.
///
/// A revision that moves one paragraph of a forty-line plan must not make the
/// King scroll forty lines to find it. Six is enough to keep a short gap
/// readable in place while folding away the long stretches that carry nothing.
pub const RUN: usize = 6;

/// A diff folded into what changed and what is merely between changes.
///
/// [`Hunk::Unchanged`] carries its lines rather than only a count, so the view
/// can expand a fold in place instead of asking the server again for text it
/// already has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hunk {
    /// Lines to read: at least one of them changed.
    Changed(Vec<DiffLine>),
    /// A stretch nothing happened in, long enough to be worth folding.
    Unchanged(Vec<DiffLine>),
}

/// Groups a diff so the view can hide the long untouched stretches.
///
/// A run of more than [`RUN`] unchanged lines becomes its own [`Hunk`]; runs
/// shorter than that stay with the changes around them, because a two-line gap
/// folded away costs a click to read something the King could already see.
pub fn hunks(lines: &[DiffLine]) -> Vec<Hunk> {
    let mut out: Vec<Hunk> = Vec::new();
    let mut buffer: Vec<DiffLine> = Vec::new();

    // Runs of `Same` are gathered, then either folded or handed back to the
    // surrounding changed hunk once we know how long the run turned out to be.
    let mut run: Vec<DiffLine> = Vec::new();

    let flush_run = |run: &mut Vec<DiffLine>, buffer: &mut Vec<DiffLine>, out: &mut Vec<Hunk>| {
        if run.len() > RUN {
            if !buffer.is_empty() {
                out.push(Hunk::Changed(std::mem::take(buffer)));
            }
            out.push(Hunk::Unchanged(std::mem::take(run)));
        } else {
            buffer.append(run);
        }
    };

    for line in lines {
        match line.change {
            Change::Same => run.push(line.clone()),
            Change::Added | Change::Removed => {
                flush_run(&mut run, &mut buffer, &mut out);
                buffer.push(line.clone());
            }
        }
    }

    flush_run(&mut run, &mut buffer, &mut out);
    if !buffer.is_empty() {
        out.push(Hunk::Changed(buffer));
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The split the whole annotation path rests on.
    ///
    /// Every construct a plan actually contains, in one document, because the
    /// failure mode is not "a block is missing" but "a block starts one line
    /// off" -- and a note pinned one line off quotes the wrong paragraph back to
    /// the model.
    #[test]
    fn a_proposal_splits_into_the_parts_the_king_argues_with() {
        let body = "# A title\n\
                    \n\
                    Some intro paragraph\n\
                    that wraps two lines.\n\
                    \n\
                    ## A section\n\
                    \n\
                    - one\n\
                    - two\n\
                    \n\
                    ```rust\n\
                    fn main() {}\n\
                    ```\n\
                    \n\
                    | a | b |\n\
                    |---|---|\n\
                    | 1 | 2 |\n\
                    \n\
                    > a quote\n\
                    \n\
                    Final para.\n";

        let found = blocks(body);
        let lines: Vec<usize> = found.iter().map(|b| b.line).collect();
        assert_eq!(lines, vec![1, 3, 6, 8, 9, 11, 15, 19, 21], "{found:#?}");

        assert_eq!(found[0].text, "# A title");
        assert_eq!(
            found[1].text, "Some intro paragraph\nthat wraps two lines.",
            "a wrapped paragraph is one thought and therefore one block"
        );
        assert!(found[5].text.starts_with("```rust"), "{:?}", found[5]);
        assert!(found[6].text.contains("| 1 | 2 |"), "{:?}", found[6]);
    }

    /// A bullet with sub-bullets is one point, not several.
    ///
    /// Folding the nesting in is what keeps the number of targets close to the
    /// number of ideas. Without it the deepest bullet in a plan becomes its own
    /// annotatable thing while the point it belongs to does not.
    #[test]
    fn a_nested_list_stays_with_the_point_it_belongs_to() {
        let body = "- one\n- two\n  - nested\n  - also nested\n- three\n";

        let found = blocks(body);
        assert_eq!(found.len(), 3, "three top-level points: {found:#?}");
        assert_eq!(found[1].line, 2);
        assert!(
            found[1].text.contains("nested"),
            "the sub-points belong to the point that carries them: {:?}",
            found[1]
        );
    }

    #[test]
    fn an_empty_proposal_has_nothing_to_annotate() {
        assert!(blocks("").is_empty());
        assert!(blocks("   \n\n  \n").is_empty());
    }

    #[test]
    fn a_revision_marks_what_moved_and_leaves_the_rest_alone() {
        let old = "# Plan\n\nFirst.\n\nSecond.\n";
        let new = "# Plan\n\nFirst, revised.\n\nSecond.\n";

        let read = diff(old, new);
        let added: Vec<&str> = read
            .iter()
            .filter(|l| l.change == Change::Added)
            .map(|l| l.text.as_str())
            .collect();
        let removed: Vec<&str> = read
            .iter()
            .filter(|l| l.change == Change::Removed)
            .map(|l| l.text.as_str())
            .collect();

        assert_eq!(added, vec!["First, revised."]);
        assert_eq!(removed, vec!["First."]);
        assert!(!unchanged(&read));
    }

    /// A revision that changed nothing must be *sayable*, not merely drawable.
    ///
    /// The view says so in a sentence. Without this the King reads a wall of
    /// unchanged lines hunting for a difference that is not there, which is the
    /// one outcome a diff exists to prevent.
    #[test]
    fn a_revision_that_changed_nothing_says_so() {
        let body = "# Plan\n\nAs before.\n";
        assert!(unchanged(&diff(body, body)));
    }

    /// Long untouched stretches fold; short gaps do not.
    ///
    /// The short-gap half is the one worth pinning: a two-line gap folded away
    /// costs the King a click to read something he could already see.
    #[test]
    fn only_the_long_untouched_stretches_are_folded() {
        let filler: String = (0..20).map(|i| format!("line {i}\n")).collect();
        let old = format!("start\n{filler}end\n");
        let new = format!("start changed\n{filler}end\n");

        let folded = hunks(&diff(&old, &new));
        assert!(
            folded
                .iter()
                .any(|h| matches!(h, Hunk::Unchanged(lines) if lines.len() > RUN)),
            "twenty untouched lines between two changes must fold: {folded:#?}"
        );

        // Two lines apart: nothing to fold, so the whole diff is one hunk to read.
        let near = hunks(&diff("a\nb\nc\n", "A\nb\nC\n"));
        assert!(
            near.iter().all(|h| matches!(h, Hunk::Changed(_))),
            "a gap shorter than the threshold stays in place: {near:#?}"
        );
    }

    /// Folding must not lose a line.
    ///
    /// The King is reading this to decide whether his notes were answered, so a
    /// dropped line is a silently wrong answer rather than a cosmetic fault.
    #[test]
    fn folding_keeps_every_line() {
        let filler: String = (0..30).map(|i| format!("line {i}\n")).collect();
        let read = diff(&format!("a\n{filler}"), &format!("b\n{filler}z\n"));

        let kept: usize = hunks(&read)
            .iter()
            .map(|h| match h {
                Hunk::Changed(lines) | Hunk::Unchanged(lines) => lines.len(),
            })
            .sum();

        assert_eq!(kept, read.len(), "every line survives being grouped");
    }
}
