//! What a plan has changed, and how one of those files differs.
//!
//! Shown to the King as the **review drawer** in the files rail, and as the
//! side-by-side diff beside the chamber. Named here for what it is.
//!
//! Pure data. Every decision that needs a disk, a repository or a diff
//! algorithm is made in `kingdom_app::review`, which is server-only; this crate
//! compiles to wasm and so may only describe the answer.
//!
//! # Why the rows are already paired
//!
//! [`DiffRow`] carries an old side and a new side together rather than a flat
//! list of tagged lines. A side-by-side view has to decide which deletion sits
//! opposite which insertion, and that decision belongs with the differ that
//! already knows a replacement was a replacement -- not with the browser, which
//! would have to reconstruct it from a sequence and would get it wrong on any
//! uneven replace. The browser renders two columns and re-decides nothing.

use crate::model::Language;
use serde::{Deserialize, Serialize};

/// Everything a plan has changed against its city's default branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeSummary {
    /// What the comparison was made against, in words the King can read --
    /// `"main"`, `"master"`, or whatever was actually found. Local branches
    /// win over remote-tracking ones, so this reads `"origin/main"` only in a
    /// clone that has no local default branch at all.
    pub base: String,
    pub files: Vec<ChangedFile>,
    /// Why the list is as it is, when that needs saying.
    ///
    /// An empty list is ambiguous: nothing changed, the workspace is gone, and
    /// the project is not a repository all render identically without this.
    pub note: Option<String>,
}

impl ChangeSummary {
    /// An answer with nothing in it, and a reason.
    pub fn nothing(base: impl Into<String>, note: impl Into<String>) -> Self {
        Self {
            base: base.into(),
            files: Vec::new(),
            note: Some(note.into()),
        }
    }

    /// Lines added across every file, for the drawer's tab badge.
    pub fn added(&self) -> u32 {
        self.files.iter().map(|f| f.added).sum()
    }

    /// Lines removed across every file.
    pub fn removed(&self) -> u32 {
        self.files.iter().map(|f| f.removed).sum()
    }
}

/// One file that differs, and by how much.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangedFile {
    /// Path relative to the plan's workspace. Identifies the file, and is what
    /// is handed back to fetch its diff.
    pub path: String,
    /// Where it used to be, for a rename. `None` otherwise.
    pub old_path: Option<String>,
    pub kind: ChangeKind,
    pub added: u32,
    pub removed: u32,
    /// True when there are no lines to count, so the row shows a word rather
    /// than a misleading `+0 -0`.
    pub binary: bool,
    /// What tints the row, reusing the map's own language colours so a `.rs`
    /// file reads the same here as it does in the tree above it.
    pub language: Language,
}

impl ChangedFile {
    /// The directory part and the file name, split for rendering: the drawer
    /// dims the folder and brightens the name, because in a narrow column the
    /// name is what is being looked for.
    pub fn split(&self) -> (&str, &str) {
        match self.path.rsplit_once('/') {
            Some((dir, name)) => (dir, name),
            None => ("", self.path.as_str()),
        }
    }
}

/// What happened to a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
    /// On disk and not in the repository at all -- the ordinary state of a file
    /// the court has just written. Kept distinct from [`ChangeKind::Added`],
    /// which git knows about and this does not.
    Untracked,
}

impl ChangeKind {
    /// The one-letter mark in the drawer's gutter, as git spells it.
    pub fn mark(&self) -> &'static str {
        match self {
            ChangeKind::Added => "A",
            ChangeKind::Modified => "M",
            ChangeKind::Deleted => "D",
            ChangeKind::Renamed => "R",
            ChangeKind::Untracked => "?",
        }
    }

    /// Said in full on hover, because a letter is a reminder and not a label.
    pub fn label(&self) -> &'static str {
        match self {
            ChangeKind::Added => "added",
            ChangeKind::Modified => "modified",
            ChangeKind::Deleted => "deleted",
            ChangeKind::Renamed => "renamed",
            ChangeKind::Untracked => "new, not yet committed",
        }
    }
}

/// One file as it stands, ready to render line by line.
///
/// What the King reads when he opens a file from the tree rather than from the
/// review drawer: not a comparison, just the file. Most files in a project have
/// no diff at all, and the tree offers all of them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceText {
    /// Path relative to the plan's workspace, as everything else here names a
    /// file.
    pub path: String,
    /// What tints the header, reusing the map's own language colours so a `.rs`
    /// file reads the same here as it does in the tree it was opened from.
    pub language: Language,
    pub lines: Vec<SourceLine>,
    /// Whether the lines below are the whole file.
    ///
    /// [`DiffVerdict`] reused rather than duplicated: its question is "is what
    /// follows the whole truth?", and every one of its answers -- binary, too
    /// large, truncated, unreadable -- reads correctly for a plain read. A
    /// second near-identical enum would be two places to add a fifth answer to.
    pub verdict: DiffVerdict,
}

/// One line of a file, as an editor would show it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceLine {
    /// The line's number in its own file, 1-based. Carried rather than derived
    /// from the index because a truncated file still has to number honestly.
    pub number: u32,
    pub text: String,
}

/// One file whole and exact, for the King to edit.
///
/// The sibling of [`SourceText`], and deliberately a second type rather than a
/// field on it, because the two answer different questions. `SourceText` is
/// *numbered, truncated, renderable* -- it is cut at a row cap so the browser
/// survives a 40,000-line file. This is *whole and byte-exact*, and a cap on it
/// would be a file saved back with its tail deleted.
///
/// # Why the text is carried rather than rebuilt from the lines
///
/// The panel already holds a [`SourceText`], and joining its lines with `\n`
/// would need no request at all. It would also be wrong: those lines come from
/// `str::lines()`, which drops the trailing newline and eats a `\r`. Rebuilt
/// and saved, every CRLF file in the project would silently become LF and every
/// file would gain or lose a final newline -- a whole-file diff the King never
/// asked for, landing in his agent's branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileText {
    /// Path relative to the plan's workspace, as everything else here names a
    /// file.
    pub path: String,
    /// What tints the header, so an editor opened on a `.rs` file reads the
    /// same as the tree it was opened from.
    pub language: Language,
    /// The file, exactly as it is on disk. Empty when [`FileText::verdict`]
    /// says it could not be read.
    pub text: String,
    /// What was on disk when this was read, so a save can tell whether it still
    /// is. See [`FileStamp`].
    pub stamp: FileStamp,
    /// Whether the text above is the whole file, and therefore whether it may
    /// be edited at all.
    ///
    /// [`DiffVerdict`] reused for [`SourceText`]'s reason. Only
    /// [`DiffVerdict::Shown`] is editable: a binary file, one too large to hold
    /// in a textarea, and one that could not be read are each a refusal with a
    /// sentence attached rather than an empty buffer to save over the original.
    pub verdict: DiffVerdict,
}

/// What a file looked like when it was read, cheaply.
///
/// The King reads a file while his agent is working in the same workspace, so
/// between opening the editor and pressing Save the court may have rewritten
/// the thing under him. Without this, that save silently destroys a round of
/// the agent's work -- the exact collision this product exists to make visible.
///
/// Length **and** hash, because either alone is a worse answer: a length misses
/// an edit that happens to preserve it, and a hash alone is one number to
/// collide on. FNV-1a is not cryptographic and does not need to be. Nothing is
/// being defended against a forger; this separates "the same bytes" from "some
/// other bytes" for one file on one machine, and the same reasoning already
/// stands behind `profile::hash`.
///
/// A file that is **not there** has a stamp too -- length zero, hash zero --
/// which is what lets a delete be checked the same way a write is, and what
/// makes deleting an already-deleted file a refusal rather than a silent
/// success.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileStamp {
    /// The file's length in bytes.
    pub bytes: u64,
    /// FNV-1a over those bytes.
    pub hash: u64,
}

impl FileStamp {
    /// The stamp of a file that is not there.
    pub const ABSENT: FileStamp = FileStamp { bytes: 0, hash: 0 };

    /// Stamps the bytes as they stand.
    ///
    /// In `kingdom-core` rather than beside the reader in `kingdom-app` on
    /// purpose: the browser holds the stamp it was given and hands it back, and
    /// a second implementation on the far side of the wire is how the two come
    /// to disagree about whether a file has moved. One function, both targets --
    /// the same reasoning [`crate::proposal`] is shared for.
    pub fn of(bytes: &[u8]) -> Self {
        // FNV-1a, 64-bit. Not cryptographic, and this is not that job.
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for b in bytes {
            hash ^= u64::from(*b);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        FileStamp {
            bytes: bytes.len() as u64,
            hash,
        }
    }
}

/// One file's difference from the base, ready to render in two columns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDiff {
    pub path: String,
    /// The same words [`ChangeSummary::base`] carries, repeated so the panel can
    /// caption itself without the drawer being open.
    pub base: String,
    pub hunks: Vec<Hunk>,
    pub verdict: DiffVerdict,
}

/// One contiguous run of changed lines and the context around it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hunk {
    pub rows: Vec<DiffRow>,
}

/// One line of the side-by-side view: what was there, and what is there now.
///
/// Both sides present is context or a replacement; one side alone is a pure
/// deletion or insertion, and the other column renders as empty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffRow {
    pub old: Option<DiffLine>,
    pub new: Option<DiffLine>,
}

impl DiffRow {
    /// Whether this row is unchanged on both sides, which is what the view
    /// renders quietly.
    pub fn is_context(&self) -> bool {
        match (&self.old, &self.new) {
            (Some(old), Some(new)) => !old.changed && !new.changed,
            _ => false,
        }
    }
}

/// One side of one row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffLine {
    /// The line's number in its own file, 1-based, as an editor counts.
    pub number: u32,
    /// The text, split so the parts that actually differ can be emphasised.
    /// Unchanged lines are one span with `emphasis` false.
    pub spans: Vec<Span>,
    /// Whether this side of the row is part of the change rather than context.
    pub changed: bool,
}

impl DiffLine {
    /// The whole line as one string, for a title attribute or a test.
    pub fn text(&self) -> String {
        self.spans.iter().map(|s| s.text.as_str()).collect()
    }
}

/// A run of text within a line, and whether it is one of the parts that differ.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub text: String,
    pub emphasis: bool,
}

/// Whether the diff below is the whole truth, and if not, why not.
///
/// Stated rather than discovered: a two-megabyte minified bundle and a PNG both
/// have a diff in principle, and rendering either one would wedge the browser.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiffVerdict {
    Shown,
    /// Not text. There is nothing to show line by line.
    Binary,
    /// Too big to diff at all, in bytes.
    TooLarge,
    /// Diffed, then cut off. Carries how many rows were dropped.
    Truncated(u32),
    /// Neither side could be read -- gone from disk, or git refused.
    Unreadable(String),
}

impl DiffVerdict {
    /// What the panel says instead of, or beneath, the rows.
    pub fn tell(&self) -> Option<String> {
        match self {
            DiffVerdict::Shown => None,
            DiffVerdict::Binary => {
                Some("This file is not text, so there is nothing to read line by line.".into())
            }
            DiffVerdict::TooLarge => Some("This file is too large to compare.".into()),
            DiffVerdict::Truncated(n) => {
                Some(format!("{n} more lines differ than are shown here."))
            }
            DiffVerdict::Unreadable(why) => Some(format!("This file could not be compared: {why}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(number: u32, text: &str, changed: bool) -> DiffLine {
        DiffLine {
            number,
            spans: vec![Span {
                text: text.to_string(),
                emphasis: false,
            }],
            changed,
        }
    }

    #[test]
    fn a_path_splits_into_a_folder_and_a_name() {
        let file = ChangedFile {
            path: "crates/kingdom-app/src/api.rs".into(),
            old_path: None,
            kind: ChangeKind::Modified,
            added: 3,
            removed: 1,
            binary: false,
            language: Language::Rust,
        };
        assert_eq!(file.split(), ("crates/kingdom-app/src", "api.rs"));

        let root = ChangedFile {
            path: "README.md".into(),
            ..file
        };
        assert_eq!(root.split(), ("", "README.md"));
    }

    /// A row with both sides unchanged is context; anything else is not, and
    /// the view tints on exactly this answer.
    #[test]
    fn context_is_both_sides_unchanged() {
        let context = DiffRow {
            old: Some(line(1, "same", false)),
            new: Some(line(1, "same", false)),
        };
        assert!(context.is_context());

        let replaced = DiffRow {
            old: Some(line(1, "before", true)),
            new: Some(line(1, "after", true)),
        };
        assert!(!replaced.is_context());

        let inserted = DiffRow {
            old: None,
            new: Some(line(1, "fresh", true)),
        };
        assert!(!inserted.is_context());
    }

    #[test]
    fn a_summary_totals_what_it_holds() {
        let file = |added, removed| ChangedFile {
            path: format!("f{added}.rs"),
            old_path: None,
            kind: ChangeKind::Modified,
            added,
            removed,
            binary: false,
            language: Language::Rust,
        };
        let summary = ChangeSummary {
            base: "main".into(),
            files: vec![file(3, 1), file(10, 4)],
            note: None,
        };
        assert_eq!(summary.added(), 13);
        assert_eq!(summary.removed(), 5);

        // An empty answer must be able to say why, or it reads as "nothing
        // changed" when it means "nothing could be read".
        let empty = ChangeSummary::nothing("main", "Not a repository.");
        assert_eq!(empty.added(), 0);
        assert!(empty.note.is_some());
    }
}
