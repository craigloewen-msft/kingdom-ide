//! The King's own edits: one file read whole, written back, or removed.
//!
//! The counterpart to [`crate::review`], which reads a file *for looking at*.
//! This reads one *for changing*, and the difference is not cosmetic:
//!
//! - **Whole, never truncated.** `review::source` cuts at `MOST_ROWS` so a
//!   40,000-line file does not become 40,000 DOM nodes. A buffer cut the same
//!   way and saved back is a file with its tail deleted, so this refuses a file
//!   it cannot carry entire rather than handing back part of one.
//! - **Byte-exact.** `review::source` splits with `str::lines()`, which drops
//!   the trailing newline and eats a `\r`. That is invisible when the lines are
//!   only being drawn and catastrophic when they are being written back: every
//!   CRLF file in the project would silently become LF. So the text crosses the
//!   wire as one `String` and returns as one, and what is written is what was
//!   shown apart from what the King typed.
//!
//! # The stamp
//!
//! The King reads a file while his agent works in the same workspace. Between
//! opening the editor and pressing Save, the court may have rewritten the thing
//! under him -- and a save that simply overwrote would destroy a round of the
//! agent's work silently, which is the exact collision this product exists to
//! surface rather than to cause. So every read carries a
//! [`kingdom_core::FileStamp`], every write and delete sends it back, and a
//! mismatch is a refusal with a sentence saying what happened.
//!
//! This is optimistic concurrency and not a lock, deliberately. A lock would
//! have to be released by something, and the something is a browser tab that
//! may simply be closed.
//!
//! # The boundary
//!
//! `path` is relative to the workspace and has already been through a
//! [`crate::tools::Sandbox`] by the time any of this is called -- see the
//! callers in `api.rs`, exactly as [`crate::review::source`] and
//! `list_directory` are. Nothing here resolves a path itself, because a second
//! resolver is a second wall to get wrong.

use crate::review::{looks_binary, MOST_BYTES};
use kingdom_core::{DiffVerdict, FileStamp, FileText, Language, Workspace};
use std::path::{Path, PathBuf};

/// One file of the plan's workspace, whole, with the stamp of what was read.
///
/// Never an error: a binary file, one too large to edit and one that is not
/// there are each an answer the panel says out loud, carried as the verdict.
/// Only [`DiffVerdict::Shown`] may be edited, and the browser is told which by
/// being handed the verdict rather than by guessing from an empty string.
pub async fn text(workspace: &Workspace, path: &str) -> FileText {
    let full = Path::new(&workspace.path).join(path);

    let refused = |verdict: DiffVerdict| FileText {
        path: path.to_string(),
        language: Language::from_path(path),
        text: String::new(),
        // A file that could not be read has no bytes to stamp, and
        // [`FileStamp::ABSENT`] is what a delete of a missing file would check
        // against. Carrying a stamp even here keeps "what did I see?" answerable
        // on every path.
        stamp: FileStamp::ABSENT,
        verdict,
    };

    // Asked before the read, as `review::source` does and for its reason: a
    // directory read as bytes is an error whose message names an errno, and "is
    // a directory" is the answer worth giving.
    match tokio::fs::metadata(&full).await {
        Ok(meta) if meta.is_dir() => {
            return refused(DiffVerdict::Unreadable(
                "that is a folder, not a file".into(),
            ));
        }
        Ok(meta) if meta.len() > MOST_BYTES => return refused(DiffVerdict::TooLarge),
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return refused(DiffVerdict::Unreadable(
                "that file is no longer in this workspace".into(),
            ));
        }
        Err(e) => return refused(DiffVerdict::Unreadable(e.to_string())),
    }

    let bytes = match tokio::fs::read(&full).await {
        Ok(bytes) => bytes,
        Err(e) => return refused(DiffVerdict::Unreadable(e.to_string())),
    };

    if looks_binary(&bytes) {
        return refused(DiffVerdict::Binary);
    }

    // The stamp is taken over the **bytes**, before any conversion, because it
    // is a fact about the file on disk rather than about the string the browser
    // was given. A file whose bytes are not valid UTF-8 would otherwise stamp
    // one thing and compare as another.
    let stamp = FileStamp::of(&bytes);

    // Lossy, and it is the reason a non-UTF-8 file must not be saved back --
    // which `looks_binary` catches for the ordinary case and
    // [`kingdom_core::FileStamp`] catches for the rest, since a lossy round trip
    // changes the bytes and therefore the stamp of what is written, not of what
    // is checked.
    let text = String::from_utf8_lossy(&bytes).into_owned();

    FileText {
        path: path.to_string(),
        language: Language::from_path(path),
        text,
        stamp,
        verdict: DiffVerdict::Shown,
    }
}

/// The King saves what he typed, if the file is still what he opened.
///
/// Returns the file as it now stands -- with its new stamp -- so the panel can
/// carry on editing without a second round trip to find out what it just wrote.
///
/// Writes through [`crate::tools::patch::write_atomically`], which is the house
/// temp-file-and-rename: a save interrupted half way leaves the old file rather
/// than a truncated one.
///
/// # Line endings are restored here, and that is not belt-and-braces
///
/// [`kingdom_core::FileText`] explains why the text is fetched whole rather than
/// rebuilt from rendered lines: `str::lines()` eats a `\r`, and a file rebuilt
/// from it saves back as a whole-file diff. Reading the bytes closes that hole
/// and leaves a *second* one open, one layer lower and invisible from the
/// server: **a DOM textarea normalises CRLF to LF in its `value`.** The bytes
/// arrive at the browser intact, the King types one character, and what comes
/// back has had every `\r` stripped by the platform before any of Kingdom's code
/// saw it.
///
/// This was caught by saving a real CRLF file through the real panel, having
/// already passed a server-side round-trip test that never went near a DOM. So
/// the convention is restored here, where the file on disk is the evidence:
/// nothing in the browser can be trusted to preserve it, and nothing in the
/// browser needs to.
pub async fn write(
    workspace: &Workspace,
    path: &str,
    content: &str,
    expected: FileStamp,
) -> Result<FileText, String> {
    let full = Path::new(&workspace.path).join(path);

    let existing = check(&full, expected).await?;
    let content = with_line_endings_of(existing.as_deref(), content);

    // Blocking, on purpose. The atomic write is `std::fs` and lives in
    // `tools/patch.rs`; wrapping it here rather than duplicating it in async
    // form keeps one implementation of the guarantee. A single source file is
    // small enough that the block is measured in microseconds.
    let target = full.clone();
    let body = content.clone();
    tokio::task::spawn_blocking(move || crate::tools::patch::write_atomically(&target, &body))
        .await
        .map_err(|e| format!("the write could not be run: {e}"))?
        .map_err(|e| format!("that file could not be written: {e}"))?;

    Ok(FileText {
        path: path.to_string(),
        language: Language::from_path(path),
        // Stamped from what was **written**, which is why the conversion above
        // happens before this and not after: the panel's next save is checked
        // against this stamp, and a stamp of the pre-conversion text would
        // refuse the King's own second edit.
        stamp: FileStamp::of(content.as_bytes()),
        text: content,
        verdict: DiffVerdict::Shown,
    })
}

/// Gives the King's text the line endings the file already used.
///
/// Two guards, and both matter:
///
/// - The file must **already** be CRLF. A project's own convention is the only
///   evidence there is of what it wants, and imposing CRLF on an LF file would
///   be the same vandalism in the other direction.
/// - The incoming text must contain **no** `\r` at all. That is the signature of
///   a browser having stripped them wholesale. Text that still has some is text
///   nothing normalised, and rewriting it would corrupt a deliberately mixed
///   file -- a test fixture *about* line endings, say.
///
/// Between them, the only case converted is the one that is certainly the
/// platform's doing rather than the King's.
fn with_line_endings_of(existing: Option<&[u8]>, content: &str) -> String {
    let Some(existing) = existing else {
        // A new file has no convention to keep. Whatever the King typed stands.
        return content.to_string();
    };

    let was_crlf = existing.windows(2).any(|w| w == b"\r\n");
    if !was_crlf || content.contains('\r') {
        return content.to_string();
    }

    content.replace('\n', "\r\n")
}

/// The King deletes the file, if it is still the one he opened.
///
/// The stamp is checked here for the same reason it is checked on a write, and
/// with one extra consequence: deleting a file the court has since rewritten is
/// the most expensive mistake available in this panel, and it is the one a
/// stale tab makes most easily.
pub async fn remove(workspace: &Workspace, path: &str, expected: FileStamp) -> Result<(), String> {
    let full = Path::new(&workspace.path).join(path);

    if tokio::fs::metadata(&full)
        .await
        .map(|m| m.is_dir())
        .unwrap_or(false)
    {
        return Err("That is a folder, not a file. Folders are not deleted here.".into());
    }

    check(&full, expected).await?;

    tokio::fs::remove_file(&full)
        .await
        .map_err(|e| format!("That file could not be deleted: {e}"))
}

/// Refuses unless the file on disk is still the one that was read, and hands
/// back what it found.
///
/// Shared by the write and the delete rather than written twice, so "has this
/// moved?" has one answer. A file that is **absent** stamps as
/// [`FileStamp::ABSENT`], which is why a delete of something already gone is
/// refused rather than quietly succeeding: the King should be told the court
/// beat him to it.
///
/// The bytes come back because the check has already paid for reading them and
/// [`write`] needs them to see what line endings the file uses. Reading twice
/// would also open a window between the two reads.
async fn check(full: &PathBuf, expected: FileStamp) -> Result<Option<Vec<u8>>, String> {
    let existing = match tokio::fs::read(full).await {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("That file could not be read back: {e}")),
    };

    let actual = existing
        .as_deref()
        .map(FileStamp::of)
        .unwrap_or(FileStamp::ABSENT);

    if actual != expected {
        return Err(
            "This file has changed on disk since you opened it \u{2014} the court \
             has been working here too. Close the file and open it again to see \
             what it says now."
                .into(),
        );
    }

    Ok(existing)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(root: &Path) -> Workspace {
        Workspace::in_place(root.to_string_lossy())
    }

    /// The whole reason the buffer is fetched rather than rebuilt from the
    /// panel's rendered lines.
    ///
    /// A CRLF file and a file with no trailing newline are the two shapes
    /// `str::lines()` quietly destroys. Opening either and saving it back
    /// untouched must leave the bytes *identical* -- anything else is a
    /// whole-file diff the King never asked for, landing in his agent's branch.
    #[tokio::test]
    async fn a_file_survives_a_round_trip_byte_for_byte() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let space = workspace(root);

        for (name, original) in [
            ("crlf.txt", "one\r\ntwo\r\n"),
            ("no-final-newline.txt", "one\ntwo"),
            ("empty.txt", ""),
            ("trailing-blank.txt", "one\n\n\n"),
        ] {
            std::fs::write(root.join(name), original).unwrap();

            let read = text(&space, name).await;
            assert_eq!(
                read.verdict,
                DiffVerdict::Shown,
                "{name} should be editable"
            );
            assert_eq!(read.text, original, "{name} must be read exactly");

            write(&space, name, &read.text, read.stamp)
                .await
                .unwrap_or_else(|e| panic!("{name} should save: {e}"));

            assert_eq!(
                std::fs::read_to_string(root.join(name)).unwrap(),
                original,
                "{name} must be unchanged by an untouched round trip"
            );
        }
    }

    /// The bug the server-side round trip above could not see.
    ///
    /// A DOM textarea normalises CRLF to LF in its `value`, so the browser hands
    /// back text with every `\r` already stripped -- by the platform, before any
    /// of Kingdom's code runs. The test above passes and the real panel still
    /// rewrote a CRLF file wholesale, which is how this was found.
    ///
    /// So this simulates the browser rather than trusting it: the text saved is
    /// what a textarea would have produced, and the file must come back with its
    /// own convention intact.
    #[tokio::test]
    async fn a_crlf_file_survives_a_browser_that_strips_its_line_endings() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let space = workspace(root);
        std::fs::write(root.join("crlf.txt"), "alpha\r\nbeta\r\ngamma").unwrap();

        let read = text(&space, "crlf.txt").await;
        assert_eq!(read.text, "alpha\r\nbeta\r\ngamma", "read exactly");

        // What a textarea gives back: the same edit, with the carriage returns
        // gone. Written by hand here because there is no DOM in a unit test --
        // which is exactly why the original test missed this.
        let from_the_browser = "alpha\nbeta\ngamma!";
        let written = write(&space, "crlf.txt", from_the_browser, read.stamp)
            .await
            .expect("the save lands");

        assert_eq!(
            std::fs::read_to_string(root.join("crlf.txt")).unwrap(),
            "alpha\r\nbeta\r\ngamma!",
            "the file's own line endings must survive the King's edit"
        );

        // And the stamp handed back must be of what was *written*, or the next
        // save is refused as stale by the King's own edit.
        write(&space, "crlf.txt", "alpha\nbeta\ngamma!!", written.stamp)
            .await
            .expect("a second save is not refused as stale");
    }

    /// The conversion is narrow on purpose, and each guard is load-bearing.
    #[test]
    fn line_endings_are_restored_only_where_the_platform_took_them() {
        // The case it exists for.
        assert_eq!(
            with_line_endings_of(Some(b"a\r\nb"), "a\nb\nc"),
            "a\r\nb\r\nc",
            "a CRLF file keeps CRLF"
        );

        // An LF project must not have CRLF imposed on it -- that is the same
        // vandalism in the other direction.
        assert_eq!(
            with_line_endings_of(Some(b"a\nb"), "a\nb\nc"),
            "a\nb\nc",
            "an LF file stays LF"
        );

        // Text that still has carriage returns is text nothing normalised, so
        // the King meant it. A fixture *about* line endings would be corrupted
        // by converting here.
        assert_eq!(
            with_line_endings_of(Some(b"a\r\nb"), "a\r\nb\nc"),
            "a\r\nb\nc",
            "deliberately mixed text is left exactly as it came"
        );

        // A new file has no convention to keep.
        assert_eq!(with_line_endings_of(None, "a\nb"), "a\nb");
    }

    /// The stamp's whole job: a save must not overwrite work it never saw.
    ///
    /// The court rewriting a file the King has open is ordinary rather than
    /// exotic -- they share one workspace by design -- so the refusal has to
    /// leave the court's work on disk untouched.
    #[tokio::test]
    async fn a_save_is_refused_when_the_file_moved_underneath() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let space = workspace(root);
        std::fs::write(root.join("f.rs"), "fn main() {}\n").unwrap();

        let read = text(&space, "f.rs").await;

        // The court gets there first.
        std::fs::write(root.join("f.rs"), "fn main() { work(); }\n").unwrap();

        let refused = write(&space, "f.rs", "fn main() { mine(); }\n", read.stamp).await;
        assert!(refused.is_err(), "a stale stamp must be refused");
        assert_eq!(
            std::fs::read_to_string(root.join("f.rs")).unwrap(),
            "fn main() { work(); }\n",
            "the refusal must leave the court's work exactly as it stands"
        );

        // And reading again gives a stamp that works, so the King's way out is
        // to reopen rather than to be stuck.
        let again = text(&space, "f.rs").await;
        write(&space, "f.rs", "fn main() { mine(); }\n", again.stamp)
            .await
            .expect("a fresh stamp saves");
    }

    /// A delete is stamped like a write, and a second one is refused rather
    /// than passing silently -- being told the file is already gone is the
    /// point.
    #[tokio::test]
    async fn a_delete_is_stamped_and_happens_once() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let space = workspace(root);
        std::fs::write(root.join("gone.rs"), "x\n").unwrap();

        let read = text(&space, "gone.rs").await;

        assert!(
            remove(&space, "gone.rs", FileStamp::ABSENT).await.is_err(),
            "a stamp that does not match the file must refuse the delete"
        );
        assert!(root.join("gone.rs").exists(), "the refusal deletes nothing");

        remove(&space, "gone.rs", read.stamp)
            .await
            .expect("the right stamp deletes");
        assert!(!root.join("gone.rs").exists());

        assert!(
            remove(&space, "gone.rs", read.stamp).await.is_err(),
            "deleting a file that is already gone must say so"
        );
    }

    /// What may not be edited says why, rather than opening an empty buffer
    /// that would save over the original.
    #[tokio::test]
    async fn what_cannot_be_edited_is_named() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let space = workspace(root);

        std::fs::write(root.join("binary.bin"), [0x00, 0x01, 0x02]).unwrap();
        std::fs::create_dir(root.join("folder")).unwrap();

        assert_eq!(
            text(&space, "binary.bin").await.verdict,
            DiffVerdict::Binary
        );
        assert!(matches!(
            text(&space, "folder").await.verdict,
            DiffVerdict::Unreadable(_)
        ));
        assert!(matches!(
            text(&space, "nothing-here.rs").await.verdict,
            DiffVerdict::Unreadable(_)
        ));

        // And each of them carries no text, so nothing can be saved back over
        // the original from a buffer that was never filled.
        assert!(text(&space, "binary.bin").await.text.is_empty());
    }

    /// A file too large to draw is too large to edit, held to `review`'s own
    /// threshold rather than to a second one.
    #[tokio::test]
    async fn a_file_too_large_to_read_is_too_large_to_edit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let space = workspace(root);

        let huge = "x".repeat(MOST_BYTES as usize + 1);
        std::fs::write(root.join("huge.txt"), &huge).unwrap();

        assert_eq!(
            text(&space, "huge.txt").await.verdict,
            DiffVerdict::TooLarge
        );
    }
}
