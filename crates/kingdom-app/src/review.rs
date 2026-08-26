//! What a plan has changed, read out of its own workspace with git.
//!
//! Server-only. Shells out to `git` for the same reason [`crate::worktree`]
//! does, and the reasoning there applies here unchanged: git's own words are
//! better than any library's translation of them, and a repository is not a
//! thing worth reimplementing.
//!
//! Everything runs in **the plan's workspace** rather than the city's checkout.
//! For an isolated plan that is its worktree, which is the whole point: the
//! files it has touched are the ones in its own checkout, and the city's
//! directory would show the King somebody else's work.
//!
//! # Why the merge base rather than the branch tip
//!
//! A plan is compared against `merge-base(default, HEAD)`, not against `main`
//! itself. `git diff main` is symmetric, so any commit that landed on main
//! *after* this worktree was cut renders as a deletion by the plan -- the King
//! would open the drawer to review his agent's work and be shown a list of
//! files it never touched. The merge base is the point the two histories
//! actually parted, and the diff from there is exactly "what this plan did".
//!
//! # Which `main`
//!
//! The King's own, not the remote's. `default_branch` tries local refs before
//! remote-tracking ones, because `origin/main` is only as fresh as the last
//! `git fetch` -- and measuring against a stale one re-introduces the very
//! false attribution the merge base is here to prevent. See the reasoning on
//! that function.
//!
//! # What counts as changed
//!
//! Committed work, uncommitted work and files never added to the repository at
//! all, because a plan's worktree is normally in all three states at once and a
//! drawer that showed only commits would be empty for most of a plan's life.
//! Untracked files are listed as [`ChangeKind::Untracked`] and counted by
//! reading them, which git will not do for a file it does not know about.

use kingdom_core::{
    ChangeKind, ChangeSummary, ChangedFile, DiffLine, DiffRow, DiffVerdict, FileDiff, Hunk,
    Language, SourceLine, SourceText, Span, Workspace,
};
use std::collections::HashMap;
use std::path::Path;

/// Lines of unchanged context kept either side of a change.
///
/// Three, as `tools/patch.rs` uses for the same reason: enough to place a hunk
/// in its function, few enough that a large file does not render whole.
const CONTEXT: usize = 3;

/// Above this, a file is reported rather than diffed.
///
/// A minified bundle or a checked-in dataset is a legitimate text file that no
/// side-by-side view can survive. 1.5 MB is comfortably above any hand-written
/// source file and comfortably below the size at which the browser stalls.
const MOST_BYTES: u64 = 1_500_000;

/// Rows one diff may render before it is cut off.
///
/// The cap is on the *rendered* rows rather than the file, because the cost the
/// King pays is DOM nodes: a 40,000-line file with one changed line is cheap,
/// and a 4,000-line file rewritten wholesale is not.
const MOST_ROWS: usize = 4_000;

/// Untracked files listed before the scan gives up.
///
/// A plan that has just run a build in its worktree can have tens of thousands
/// of untracked files if the project's ignore rules do not cover the output.
/// The drawer is for reviewing work, and a list that long is not reviewable.
const MOST_UNTRACKED: usize = 500;

/// Everything this plan has changed, as the drawer shows it.
///
/// Never an `Err` for an ordinary absence -- a workspace that is gone, a project
/// with no git, a repository with no default branch all come back as an empty
/// summary carrying a note. The drawer's whole job is to say what is true, and
/// "this is not a repository" is an answer rather than a failure.
pub async fn changes(workspace: &Workspace) -> ChangeSummary {
    let root = Path::new(&workspace.path);

    if !root.is_dir() {
        return ChangeSummary::nothing(
            "—",
            "This plan's workspace is no longer on disk, so there is nothing to compare.",
        );
    }
    if git(root, &["rev-parse", "--is-inside-work-tree"])
        .await
        .is_err()
    {
        return ChangeSummary::nothing("—", "This project is not a git repository.");
    }

    let Some(against) = default_branch(root, workspace).await else {
        return ChangeSummary::nothing(
            "—",
            "This repository has no commits yet, so there is nothing to compare against.",
        );
    };

    let mut files = tracked(root, &against.commit).await;
    let (untracked, capped) = untracked(root).await;
    files.extend(untracked);

    // One order for the whole list, so a file does not move when it stops being
    // untracked and starts being a commit.
    files.sort_by_key(|a| a.path.to_lowercase());

    let mut note = against.note;
    if capped {
        let more = format!(
            "More than {MOST_UNTRACKED} files here are not in the repository, so only the \
             first are listed."
        );
        note = Some(match note {
            Some(existing) => format!("{existing} {more}"),
            None => more,
        });
    }

    ChangeSummary {
        base: against.label,
        files,
        note,
    }
}

/// One file's difference from the base, paired into side-by-side rows.
///
/// `path` is relative to the workspace and has already been through a
/// [`crate::tools::Sandbox`] by the time this is called -- see the caller in
/// `api.rs`, which is where the boundary is enforced, exactly as it is for
/// `list_directory`.
pub async fn diff(workspace: &Workspace, path: &str) -> FileDiff {
    let root = Path::new(&workspace.path);

    let against = match default_branch(root, workspace).await {
        Some(against) => against,
        None => {
            return FileDiff {
                path: path.to_string(),
                base: "—".into(),
                hunks: Vec::new(),
                verdict: DiffVerdict::Unreadable(
                    "there is nothing in this repository to compare against".into(),
                ),
            }
        }
    };

    let before = match blob(root, &against.commit, path).await {
        // A file absent from the base is not an error: it is a new file, and
        // its whole content is the insertion.
        Ok(found) => found.unwrap_or_default(),
        Err(why) => {
            return FileDiff {
                path: path.to_string(),
                base: against.label,
                hunks: Vec::new(),
                verdict: DiffVerdict::Unreadable(why),
            }
        }
    };

    let full = root.join(path);
    // A file deleted by the plan is empty on the new side rather than
    // unreadable -- the deletion is the thing being reviewed.
    let after = match std::fs::read(&full) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => {
            return FileDiff {
                path: path.to_string(),
                base: against.label,
                hunks: Vec::new(),
                verdict: DiffVerdict::Unreadable(e.to_string()),
            }
        }
    };

    if before.len() as u64 > MOST_BYTES || after.len() as u64 > MOST_BYTES {
        return FileDiff {
            path: path.to_string(),
            base: against.label,
            hunks: Vec::new(),
            verdict: DiffVerdict::TooLarge,
        };
    }
    if looks_binary(&before) || looks_binary(&after) {
        return FileDiff {
            path: path.to_string(),
            base: against.label,
            hunks: Vec::new(),
            verdict: DiffVerdict::Binary,
        };
    }

    let before = String::from_utf8_lossy(&before).into_owned();
    let after = String::from_utf8_lossy(&after).into_owned();
    let (hunks, verdict) = pair(&before, &after);

    FileDiff {
        path: path.to_string(),
        base: against.label,
        hunks,
        verdict,
    }
}

/// One file as it stands in the plan's workspace, line by line.
///
/// **No git.** This is the file as it *is*, not as it differs, which is the
/// whole reason it exists beside [`diff`]: most files in a project have never
/// been touched by the plan, and the tree offers all of them. Reading through
/// git would give an empty answer for exactly those.
///
/// Shares [`diff`]'s guards rather than inventing its own -- [`MOST_BYTES`],
/// [`looks_binary`] and [`MOST_ROWS`] -- because "can the browser survive
/// rendering this?" is the same question whichever panel is asking, and a
/// second set of thresholds would be a second place to get it wrong.
///
/// `path` is relative to the workspace and has already been through a
/// [`crate::tools::Sandbox`] by the time this is called -- see the caller in
/// `api.rs`, which is where the boundary is enforced, exactly as it is for
/// [`diff`] and `list_directory`.
pub async fn source(workspace: &Workspace, path: &str) -> SourceText {
    let full = Path::new(&workspace.path).join(path);

    let unreadable = |why: String| SourceText {
        path: path.to_string(),
        language: Language::from_path(path),
        lines: Vec::new(),
        verdict: DiffVerdict::Unreadable(why),
    };

    // Asked before the read rather than after it: a directory read as bytes is
    // an error whose message names an errno, and "is a directory" is the
    // answer worth giving.
    match tokio::fs::metadata(&full).await {
        Ok(meta) if meta.is_dir() => {
            return unreadable("that is a folder, not a file".into());
        }
        Ok(meta) if meta.len() > MOST_BYTES => {
            return SourceText {
                path: path.to_string(),
                language: Language::from_path(path),
                lines: Vec::new(),
                verdict: DiffVerdict::TooLarge,
            };
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Named plainly rather than reported as an io error. The ordinary
            // way to reach this is a file the court deleted while the King had
            // it open, and "No such file or directory (os error 2)" describes
            // that badly.
            return unreadable("that file is no longer in this workspace".into());
        }
        Err(e) => return unreadable(e.to_string()),
    }

    let bytes = match tokio::fs::read(&full).await {
        Ok(bytes) => bytes,
        Err(e) => return unreadable(e.to_string()),
    };

    if looks_binary(&bytes) {
        return SourceText {
            path: path.to_string(),
            language: Language::from_path(path),
            lines: Vec::new(),
            verdict: DiffVerdict::Binary,
        };
    }

    let text = String::from_utf8_lossy(&bytes);
    // `lines()` and not `split('\n')`: a trailing newline ends the last line
    // rather than opening an empty one, which is how an editor numbers a file
    // and how `count_lines` already counts it.
    let all: Vec<&str> = text.lines().collect();

    let dropped = all.len().saturating_sub(MOST_ROWS);
    let lines = all
        .into_iter()
        .take(MOST_ROWS)
        .enumerate()
        .map(|(i, text)| SourceLine {
            // 1-based, as an editor counts -- and as a note against it will
            // name it back to the court.
            number: (i + 1) as u32,
            text: text.to_string(),
        })
        .collect();

    SourceText {
        path: path.to_string(),
        language: Language::from_path(path),
        lines,
        verdict: if dropped > 0 {
            DiffVerdict::Truncated(dropped as u32)
        } else {
            DiffVerdict::Shown
        },
    }
}

// -- The comparison point -----------------------------------------------------

/// Where a plan's work is measured from, and what to call it.
struct Against {
    /// The commit every diff is taken against: the merge base, usually.
    commit: String,
    /// What the King is told it is -- the branch's name, not a hash.
    label: String,
    /// Said only when the answer is not the obvious one.
    note: Option<String>,
}

/// Finds the branch this repository considers its default, and the point this
/// plan's history parted from it.
///
/// **Local refs are tried before remote-tracking ones, and that is the whole
/// point.** A remote-tracking ref is only as fresh as the last `git fetch`,
/// while local `main` is what the King actually merges into. Preferring
/// `origin/main` measures a plan against the last fetch, so every commit that
/// has landed on local `main` since then renders as the plan's own work -- the
/// same false attribution the merge base exists to prevent, arriving by way of
/// the ref rather than the comparison. A test pins it.
///
/// So the order is: the local branch `origin/HEAD` names (with the `origin/`
/// stripped, so a repository whose default is `develop` still gets local
/// `develop`), then the `main`/`master` conventions, then the branch this
/// workspace was actually cut from.
///
/// The remote-tracking refs are kept *beneath* all of those rather than
/// dropped. A clone that only ever checked out a feature branch has no local
/// default at all, and there `origin/main` is the best answer available --
/// stale by a fetch, but far better than narrowing the drawer to uncommitted
/// work. Failing everything, `HEAD` does exactly that and says so, which is a
/// smaller true answer rather than a larger false one.
async fn default_branch(root: &Path, workspace: &Workspace) -> Option<Against> {
    // What the remote declares its default to be, e.g. `origin/main`. Used for
    // the *name* first and only as a ref later.
    let declared = git(
        root,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    )
    .await
    .ok()
    .map(|d| d.trim().to_string())
    .filter(|d| !d.is_empty());

    let mut candidates: Vec<String> = Vec::new();

    // -- Local, in order of authority ----------------------------------------
    if let Some(declared) = &declared {
        // `origin/main` -> `main`: the local branch the remote's default names.
        let local = declared.strip_prefix("origin/").unwrap_or(declared);
        candidates.push(local.to_string());
    }
    for usual in ["main", "master"] {
        candidates.push(usual.to_string());
    }
    if let Some(base) = &workspace.base {
        candidates.push(base.clone());
    }

    // -- Remote-tracking, only once no local ref answered --------------------
    if let Some(declared) = &declared {
        candidates.push(declared.clone());
    }
    for usual in ["main", "master"] {
        candidates.push(format!("origin/{usual}"));
    }
    if let Some(base) = &workspace.base {
        candidates.push(format!("origin/{base}"));
    }

    for name in candidates {
        // `refs/heads/` and `refs/remotes/` both resolve here, but a local
        // branch is verified as a branch rather than as any object that happens
        // to share the name.
        let Ok(resolved) = git(root, &["rev-parse", "--verify", "--quiet", &name]).await else {
            continue;
        };
        let resolved = resolved.trim().to_string();
        if resolved.is_empty() {
            continue;
        }

        // The point the histories parted. Unrelated histories have no merge
        // base at all, and there the ref itself is the honest comparison.
        let commit = match git(root, &["merge-base", &resolved, "HEAD"]).await {
            Ok(base) if !base.trim().is_empty() => base.trim().to_string(),
            _ => resolved,
        };

        return Some(Against {
            commit,
            label: name,
            note: None,
        });
    }

    // No default branch anywhere. HEAD still gives the King his uncommitted
    // work, which is better than an empty drawer, but he must be told that is
    // all he is seeing.
    let head = git(root, &["rev-parse", "--verify", "--quiet", "HEAD"])
        .await
        .ok()?;
    let head = head.trim().to_string();
    if head.is_empty() {
        return None;
    }

    Some(Against {
        commit: head,
        label: "this branch".into(),
        note: Some(
            "No main or master branch was found, so this shows only work not yet committed.".into(),
        ),
    })
}

// -- The list -----------------------------------------------------------------

/// Files git knows about that differ from the base.
///
/// Two commands rather than one: `--numstat` has the counts and `--name-status`
/// has the kind, and no single porcelain gives both in a form that is safe to
/// parse. Both are `-z`, so a path with a space or a newline in it survives,
/// and both are `--relative`, so a city that is a subdirectory of a larger
/// repository reports paths the workspace can actually open.
async fn tracked(root: &Path, base: &str) -> Vec<ChangedFile> {
    let Ok(numstat) = git(root, &["diff", "--numstat", "-z", "-M", "--relative", base]).await
    else {
        return Vec::new();
    };
    let kinds = match git(
        root,
        &["diff", "--name-status", "-z", "-M", "--relative", base],
    )
    .await
    {
        Ok(raw) => name_status(&raw),
        Err(_) => HashMap::new(),
    };

    numstat_rows(&numstat)
        .into_iter()
        .map(|row| {
            let kind = kinds
                .get(&row.path)
                .copied()
                .unwrap_or(ChangeKind::Modified);
            ChangedFile {
                language: Language::from_path(&row.path),
                path: row.path,
                old_path: row.old_path,
                kind,
                added: row.added,
                removed: row.removed,
                binary: row.binary,
            }
        })
        .collect()
}

/// One row of `--numstat`, before its kind is known.
struct Numstat {
    path: String,
    old_path: Option<String>,
    added: u32,
    removed: u32,
    binary: bool,
}

/// Parses `diff --numstat -z -M`.
///
/// The format is `added \t removed \t path \0`, except for a rename, which is
/// `added \t removed \t \0 old \0 new \0` -- the path field is *empty* and two
/// more records follow. Getting that wrong shifts every subsequent file by one,
/// which is why it is parsed as a stream rather than split into lines.
/// A binary file reports `-` for both counts.
fn numstat_rows(raw: &str) -> Vec<Numstat> {
    let mut fields = raw.split('\0').peekable();
    let mut rows = Vec::new();

    while let Some(record) = fields.next() {
        if record.is_empty() {
            continue;
        }
        let mut parts = record.splitn(3, '\t');
        let (Some(added), Some(removed), Some(path)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };

        let binary = added == "-" || removed == "-";
        let added = added.parse().unwrap_or(0);
        let removed = removed.parse().unwrap_or(0);

        // An empty path is the rename form: the old and new names are the next
        // two records.
        let (path, old_path) = if path.is_empty() {
            let old = fields.next().unwrap_or_default().to_string();
            let new = fields.next().unwrap_or_default().to_string();
            (new, Some(old))
        } else {
            (path.to_string(), None)
        };

        if path.is_empty() {
            continue;
        }

        rows.push(Numstat {
            path,
            old_path,
            added,
            removed,
            binary,
        });
    }

    rows
}

/// Parses `diff --name-status -z -M` into a path -> kind map.
///
/// Same stream shape: a status letter, then one path, except `R`/`C` which are
/// followed by two. The map is keyed on the *new* path, which is what the rest
/// of this module calls a file.
fn name_status(raw: &str) -> HashMap<String, ChangeKind> {
    let mut fields = raw.split('\0');
    let mut kinds = HashMap::new();

    while let Some(status) = fields.next() {
        if status.is_empty() {
            continue;
        }
        let letter = status.chars().next().unwrap_or('M');
        let renamed = matches!(letter, 'R' | 'C');

        let first = fields.next().unwrap_or_default();
        let path = if renamed {
            fields.next().unwrap_or_default()
        } else {
            first
        };
        if path.is_empty() {
            continue;
        }

        kinds.insert(
            path.to_string(),
            match letter {
                'A' => ChangeKind::Added,
                'D' => ChangeKind::Deleted,
                'R' | 'C' => ChangeKind::Renamed,
                _ => ChangeKind::Modified,
            },
        );
    }

    kinds
}

/// Files on disk that the repository has never been told about.
///
/// Counted by reading them, because git will not count a file it does not
/// track. Returns whether the scan was capped, so the drawer can say the list
/// is partial rather than quietly shortening it.
async fn untracked(root: &Path) -> (Vec<ChangedFile>, bool) {
    let Ok(raw) = git(root, &["ls-files", "--others", "--exclude-standard", "-z"]).await else {
        return (Vec::new(), false);
    };

    let paths: Vec<&str> = raw.split('\0').filter(|p| !p.is_empty()).collect();
    let capped = paths.len() > MOST_UNTRACKED;

    let listed = paths
        .into_iter()
        .take(MOST_UNTRACKED)
        .map(|path| {
            let full = root.join(path);
            let bytes = std::fs::read(&full).unwrap_or_default();
            let oversized = bytes.len() as u64 > MOST_BYTES;
            let binary = oversized || looks_binary(&bytes);

            ChangedFile {
                language: Language::from_path(path),
                path: path.to_string(),
                old_path: None,
                kind: ChangeKind::Untracked,
                // Every line is an addition: the file did not exist before.
                added: if binary { 0 } else { count_lines(&bytes) },
                removed: 0,
                binary,
            }
        })
        .collect();

    (listed, capped)
}

/// Lines as git counts them for `--numstat`: a trailing newline does not open a
/// new line, and a file with no trailing newline still ends one.
fn count_lines(bytes: &[u8]) -> u32 {
    if bytes.is_empty() {
        return 0;
    }
    let newlines = bytes.iter().filter(|b| **b == b'\n').count();
    let unterminated = !bytes.ends_with(b"\n");
    (newlines + usize::from(unterminated)) as u32
}

/// git's own heuristic, near enough: a NUL byte in the first block means not
/// text. Cheap, and wrong only for files no diff would help with anyway.
fn looks_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8_000).any(|b| *b == 0)
}

// -- The pairing --------------------------------------------------------------

/// Turns two texts into side-by-side rows.
///
/// The whole reason this happens on the server: a `Replace` op knows that a run
/// of deletions and a run of insertions are *the same lines, rewritten*, and
/// zips them into paired rows with inline emphasis. A browser handed a flat
/// sequence of tagged lines would have to guess at that, and would guess wrong
/// whenever the two runs are of different lengths.
fn pair(before: &str, after: &str) -> (Vec<Hunk>, DiffVerdict) {
    use similar::{ChangeTag, TextDiff};

    let diff = TextDiff::from_lines(before, after);
    let mut hunks = Vec::new();
    let mut rows_drawn = 0usize;
    let mut dropped = 0u32;

    for group in diff.grouped_ops(CONTEXT) {
        let mut rows: Vec<DiffRow> = Vec::new();

        for op in &group {
            // Held back so a replacement's deletions and insertions can be
            // zipped together rather than stacked one after the other.
            let mut deleted: Vec<DiffLine> = Vec::new();
            let mut inserted: Vec<DiffLine> = Vec::new();

            for change in diff.iter_inline_changes(op) {
                let spans: Vec<Span> = change
                    .iter_strings_lossy()
                    .map(|(emphasis, text)| Span {
                        text: text.trim_end_matches(['\n', '\r']).to_string(),
                        emphasis,
                    })
                    .filter(|span| !span.text.is_empty())
                    .collect();

                match change.tag() {
                    ChangeTag::Equal => {
                        let number = change.old_index().unwrap_or(0) as u32 + 1;
                        let new_number = change.new_index().unwrap_or(0) as u32 + 1;
                        rows.push(DiffRow {
                            old: Some(DiffLine {
                                number,
                                spans: spans.clone(),
                                changed: false,
                            }),
                            new: Some(DiffLine {
                                number: new_number,
                                spans,
                                changed: false,
                            }),
                        });
                    }
                    ChangeTag::Delete => deleted.push(DiffLine {
                        number: change.old_index().unwrap_or(0) as u32 + 1,
                        spans,
                        changed: true,
                    }),
                    ChangeTag::Insert => inserted.push(DiffLine {
                        number: change.new_index().unwrap_or(0) as u32 + 1,
                        spans,
                        changed: true,
                    }),
                }
            }

            // Zipped, then whatever is left over on either side stands alone.
            // An uneven replace -- three lines becoming one -- is the case a
            // client-side pairing gets wrong.
            let mut old_side = deleted.into_iter();
            let mut new_side = inserted.into_iter();
            loop {
                match (old_side.next(), new_side.next()) {
                    (None, None) => break,
                    (old, new) => rows.push(DiffRow { old, new }),
                }
            }
        }

        // Counted as it goes rather than trimmed at the end, so a file that is
        // one enormous rewrite stops costing at the cap instead of being built
        // whole and then thrown away.
        if rows_drawn >= MOST_ROWS {
            dropped += rows.len() as u32;
            continue;
        }
        if rows_drawn + rows.len() > MOST_ROWS {
            let keep = MOST_ROWS - rows_drawn;
            dropped += (rows.len() - keep) as u32;
            rows.truncate(keep);
        }

        rows_drawn += rows.len();
        if !rows.is_empty() {
            hunks.push(Hunk { rows });
        }
    }

    let verdict = if dropped > 0 {
        DiffVerdict::Truncated(dropped)
    } else {
        DiffVerdict::Shown
    };

    (hunks, verdict)
}

// -- git ----------------------------------------------------------------------

/// One file's content at a commit, or `None` if it was not there.
///
/// `rev-parse` first, then `cat-file`, rather than `git show <rev>:<path>`:
/// "that path did not exist" and "git failed" are different answers -- the
/// first is an ordinary new file and the second is worth telling the King about
/// -- and `show` reports both as a fatal error on stderr. Raw bytes, so the
/// binary check below sees what is actually there.
///
/// The path is spelled `<rev>:./<path>` so it is read relative to the current
/// directory, which is what makes a city nested inside a larger repository work.
async fn blob(root: &Path, commit: &str, path: &str) -> Result<Option<Vec<u8>>, String> {
    let spec = format!("{commit}:./{path}");

    let Ok(object) = git(root, &["rev-parse", "--verify", "--quiet", &spec]).await else {
        return Ok(None);
    };
    let object = object.trim();
    if object.is_empty() {
        return Ok(None);
    }

    let output = tokio::process::Command::new("git")
        .args(["cat-file", "blob", object])
        .current_dir(root)
        .output()
        .await
        .map_err(|e| e.to_string())?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }

    Ok(Some(output.stdout))
}

/// Runs one git command in a repository, returning its stdout.
///
/// A twin of [`crate::worktree`]'s helper rather than a share of it: that one
/// returns its own error type, which carries refusals this module has no use
/// for, and this one is only ever asked whether the command worked.
async fn git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let output = tokio::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .await
        .map_err(|e| e.to_string())?;

    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if message.is_empty() {
            format!("`git {}` failed", args.join(" "))
        } else {
            message
        });
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A repository with one commit on `main`, and a plan working on a branch
    /// cut from it -- the shape every test here needs.
    async fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        for args in [
            vec!["init", "-q", "-b", "main", "."],
            vec!["config", "user.email", "court@kingdom"],
            vec!["config", "user.name", "The Court"],
        ] {
            git(root, &args).await.expect("git setup");
        }
        std::fs::write(root.join("keep.rs"), "a\nb\nc\n").unwrap();
        std::fs::write(root.join("gone.rs"), "x\ny\n").unwrap();
        git(root, &["add", "."]).await.unwrap();
        git(root, &["commit", "-m", "first"]).await.unwrap();
        dir
    }

    async fn commit(root: &Path, message: &str) {
        git(root, &["add", "-A"]).await.unwrap();
        git(root, &["commit", "-m", message]).await.unwrap();
    }

    fn workspace(root: &Path) -> Workspace {
        Workspace::in_place(root.to_string_lossy())
    }

    fn find<'a>(summary: &'a ChangeSummary, path: &str) -> &'a ChangedFile {
        summary
            .files
            .iter()
            .find(|f| f.path == path)
            .unwrap_or_else(|| panic!("{path} missing from {:?}", summary.files))
    }

    /// The drawer's whole promise, in one test: committed, uncommitted and
    /// never-committed work all appear, each with the counts git itself gives.
    #[tokio::test]
    async fn every_kind_of_change_is_listed() {
        let dir = repo().await;
        let root = dir.path();
        git(root, &["checkout", "-q", "-b", "kingdom/work"])
            .await
            .unwrap();

        std::fs::write(root.join("keep.rs"), "a\nB\nc\nd\n").unwrap();
        std::fs::remove_file(root.join("gone.rs")).unwrap();
        std::fs::write(root.join("added.rs"), "one\ntwo\n").unwrap();
        commit(root, "the work").await;

        // Written after the commit, and never added at all: the two states a
        // plan's worktree is normally in.
        std::fs::write(root.join("keep.rs"), "a\nB\nc\nd\ne\n").unwrap();
        std::fs::write(root.join("loose.rs"), "fresh\n").unwrap();

        let summary = changes(&workspace(root)).await;

        assert_eq!(summary.base, "main");
        assert_eq!(find(&summary, "keep.rs").kind, ChangeKind::Modified);
        assert_eq!(
            find(&summary, "keep.rs").added,
            3,
            "uncommitted work counts"
        );
        assert_eq!(find(&summary, "gone.rs").kind, ChangeKind::Deleted);
        assert_eq!(find(&summary, "added.rs").kind, ChangeKind::Added);
        assert_eq!(find(&summary, "loose.rs").kind, ChangeKind::Untracked);
        assert_eq!(
            find(&summary, "loose.rs").added,
            1,
            "an untracked file is counted by reading it, since git will not"
        );
        assert!(summary.note.is_none(), "{:?}", summary.note);
    }

    /// The measured reason for the merge base.
    ///
    /// With `git diff main`, commits that landed on main after the worktree was
    /// cut render as deletions *by the plan* -- the King opens the drawer to
    /// review his agent and is shown files it never touched.
    #[tokio::test]
    async fn work_that_landed_on_main_afterwards_is_not_the_plans() {
        let dir = repo().await;
        let root = dir.path();
        git(root, &["checkout", "-q", "-b", "kingdom/work"])
            .await
            .unwrap();
        std::fs::write(root.join("keep.rs"), "a\nB\nc\n").unwrap();
        commit(root, "the plan's work").await;

        // main moves on underneath, as it does while an agent is running.
        git(root, &["checkout", "-q", "main"]).await.unwrap();
        std::fs::write(root.join("elsewhere.rs"), "not the plan's\n").unwrap();
        commit(root, "somebody else").await;
        git(root, &["checkout", "-q", "kingdom/work"])
            .await
            .unwrap();

        let summary = changes(&workspace(root)).await;
        let touched: Vec<&str> = summary.files.iter().map(|f| f.path.as_str()).collect();

        assert_eq!(
            touched,
            vec!["keep.rs"],
            "only this plan's own work belongs in its review drawer"
        );
    }

    /// The King's own `main`, not the last fetch.
    ///
    /// `origin/HEAD` names `origin/main` in any ordinary clone, and a
    /// remote-tracking ref is only as fresh as the last `git fetch`. Measuring
    /// against it attributes every commit that has landed on local `main` since
    /// then to whichever plan the King opens -- the same false-attribution
    /// failure the merge base exists to prevent, arriving by way of the ref
    /// instead of the comparison.
    #[tokio::test]
    async fn a_stale_remote_ref_does_not_become_the_comparison() {
        let dir = repo().await;
        let root = dir.path();

        // A remote-tracking ref pinned to the first commit, as a clone that has
        // not fetched in a while would have. No network: the refs are written
        // directly, which is all a real fetch would have left behind.
        git(root, &["update-ref", "refs/remotes/origin/main", "HEAD"])
            .await
            .unwrap();
        git(
            root,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        )
        .await
        .unwrap();

        // Work lands on local main afterwards -- a merge by the King, or a pull.
        std::fs::write(root.join("elsewhere.rs"), "somebody else's\n").unwrap();
        commit(root, "landed on main since the last fetch").await;

        git(root, &["checkout", "-q", "-b", "kingdom/work"])
            .await
            .unwrap();
        std::fs::write(root.join("keep.rs"), "a\nB\nc\n").unwrap();
        commit(root, "the plan's work").await;

        let summary = changes(&workspace(root)).await;
        let touched: Vec<&str> = summary.files.iter().map(|f| f.path.as_str()).collect();

        assert_eq!(
            summary.base, "main",
            "the local branch is what the King merges into"
        );
        assert_eq!(
            touched,
            vec!["keep.rs"],
            "work already on local main is not this plan's, however stale the remote is"
        );
    }

    /// `master` is found when there is no `main`, and what the drawer calls the
    /// comparison is the branch's real name.
    #[tokio::test]
    async fn an_older_default_branch_is_still_found() {
        let dir = repo().await;
        let root = dir.path();
        git(root, &["branch", "-m", "main", "master"])
            .await
            .unwrap();
        git(root, &["checkout", "-q", "-b", "kingdom/work"])
            .await
            .unwrap();
        std::fs::write(root.join("keep.rs"), "a\nB\nc\n").unwrap();

        let summary = changes(&workspace(root)).await;
        assert_eq!(summary.base, "master");
        assert_eq!(summary.files.len(), 1);
    }

    /// A repository with neither convention still shows uncommitted work, and
    /// says that is all it is showing.
    #[tokio::test]
    async fn a_repository_with_no_usual_default_says_what_it_is_showing() {
        let dir = repo().await;
        let root = dir.path();
        git(root, &["branch", "-m", "main", "trunk"]).await.unwrap();
        std::fs::write(root.join("keep.rs"), "a\nB\nc\n").unwrap();

        let summary = changes(&workspace(root)).await;
        assert_eq!(summary.base, "this branch");
        assert!(
            summary
                .note
                .is_some_and(|n| n.contains("not yet committed")),
            "a narrowed comparison must say so"
        );
    }

    /// An empty list is ambiguous, so each way of reaching one carries its own
    /// note rather than reading as "nothing changed".
    #[tokio::test]
    async fn an_empty_answer_says_why() {
        let plain = tempfile::tempdir().unwrap();
        let summary = changes(&workspace(plain.path())).await;
        assert!(summary.files.is_empty());
        assert!(
            summary
                .note
                .is_some_and(|n| n.contains("not a git repository")),
            "a project with no git must say so"
        );

        let gone = plain.path().join("never-existed");
        let summary = changes(&Workspace::in_place(gone.to_string_lossy())).await;
        assert!(
            summary
                .note
                .is_some_and(|n| n.contains("no longer on disk")),
            "a workspace that has been disposed of must say so"
        );
    }

    /// A binary file is named rather than rendered, both in the list and in the
    /// panel -- a PNG drawn as text is worse than a sentence.
    #[tokio::test]
    async fn a_binary_file_is_reported_rather_than_rendered() {
        let dir = repo().await;
        let root = dir.path();
        git(root, &["checkout", "-q", "-b", "kingdom/work"])
            .await
            .unwrap();
        std::fs::write(root.join("seal.png"), [0x89, 0x50, 0x00, 0x01, 0x02]).unwrap();
        commit(root, "a picture").await;

        let summary = changes(&workspace(root)).await;
        assert!(find(&summary, "seal.png").binary);

        let diff = diff(&workspace(root), "seal.png").await;
        assert_eq!(diff.verdict, DiffVerdict::Binary);
        assert!(diff.hunks.is_empty());
    }

    /// The pairing, end to end against a real repository: a changed line sits
    /// opposite the line it replaced, with the differing words emphasised.
    #[tokio::test]
    async fn a_replacement_is_paired_side_by_side() {
        let dir = repo().await;
        let root = dir.path();
        std::fs::write(root.join("keep.rs"), "a\nB\nc\n").unwrap();

        let diff = diff(&workspace(root), "keep.rs").await;
        assert_eq!(diff.verdict, DiffVerdict::Shown);

        let rows: Vec<&DiffRow> = diff.hunks.iter().flat_map(|h| &h.rows).collect();
        let changed = rows
            .iter()
            .find(|r| !r.is_context())
            .expect("the changed line");

        assert_eq!(changed.old.as_ref().unwrap().text(), "b");
        assert_eq!(changed.new.as_ref().unwrap().text(), "B");
        assert_eq!(
            changed.old.as_ref().unwrap().number,
            2,
            "1-based, as an editor counts"
        );
    }

    /// A file absent from the base is not an error -- it is a new file, and its
    /// whole content is the insertion.
    #[tokio::test]
    async fn a_new_file_renders_entirely_on_the_new_side() {
        let dir = repo().await;
        let root = dir.path();
        std::fs::write(root.join("fresh.rs"), "one\ntwo\n").unwrap();

        let diff = diff(&workspace(root), "fresh.rs").await;
        assert_eq!(diff.verdict, DiffVerdict::Shown);

        let rows: Vec<&DiffRow> = diff.hunks.iter().flat_map(|h| &h.rows).collect();
        assert_eq!(rows.len(), 2);
        assert!(
            rows.iter().all(|r| r.old.is_none() && r.new.is_some()),
            "nothing was there before, so the old column is empty throughout"
        );
    }

    /// And the other direction: a deleted file is all deletions rather than
    /// unreadable, because the deletion is the thing under review.
    #[tokio::test]
    async fn a_deleted_file_renders_entirely_on_the_old_side() {
        let dir = repo().await;
        let root = dir.path();
        std::fs::remove_file(root.join("gone.rs")).unwrap();

        let diff = diff(&workspace(root), "gone.rs").await;
        assert_eq!(diff.verdict, DiffVerdict::Shown);

        let rows: Vec<&DiffRow> = diff.hunks.iter().flat_map(|h| &h.rows).collect();
        assert!(!rows.is_empty());
        assert!(rows.iter().all(|r| r.new.is_none() && r.old.is_some()));
    }

    /// A rename arrives from `--numstat -z -M` as an empty path field followed
    /// by two more records, and a binary file as `-` for both counts. The
    /// fixture is pinned against real git output by the test below it, because
    /// misreading either shape shifts every later file by one.
    #[test]
    fn a_rename_does_not_shift_the_files_after_it() {
        // One record per line, each NUL-terminated. Split at the record
        // boundaries rather than written as one run: `\03` reads as an octal
        // escape at a glance, when it is a NUL terminator followed by the digit
        // that opens the next record.
        let raw = concat!(
            "1\t1\t\0old/name.rs\0new/name.rs\0",
            "-\t-\tseal.png\0",
            "3\t0\tafter.rs\0",
        );
        let rows = numstat_rows(raw);

        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].path, "new/name.rs");
        assert_eq!(rows[0].old_path.as_deref(), Some("old/name.rs"));
        assert!(rows[1].binary, "`-` counts mean a binary file");
        assert_eq!(rows[1].path, "seal.png");
        assert_eq!(
            rows[2].path, "after.rs",
            "the file after a rename is not lost"
        );
        assert_eq!(rows[2].added, 3);
    }

    /// And the same two shapes as git itself actually emits them, so the
    /// hand-written fixture above cannot drift away from the real format.
    #[tokio::test]
    async fn git_spells_a_rename_and_a_binary_file_the_way_the_parser_expects() {
        let dir = repo().await;
        let root = dir.path();
        git(root, &["checkout", "-q", "-b", "kingdom/work"])
            .await
            .unwrap();
        git(root, &["mv", "keep.rs", "moved.rs"]).await.unwrap();
        std::fs::write(root.join("moved.rs"), "a\nB\nc\n").unwrap();
        std::fs::write(root.join("seal.png"), [0x89, 0x50, 0x00, 0x01]).unwrap();
        commit(root, "moved and drew").await;

        let summary = changes(&workspace(root)).await;

        let moved = find(&summary, "moved.rs");
        assert_eq!(moved.kind, ChangeKind::Renamed);
        assert_eq!(moved.old_path.as_deref(), Some("keep.rs"));
        assert!(find(&summary, "seal.png").binary);
        assert!(
            !summary.files.iter().any(|f| f.path == "gone.rs"),
            "an untouched file must not appear at all: {:?}",
            summary.files
        );
    }

    /// `--name-status -z -M` has the same two-record shape for a rename, keyed
    /// on the new path because that is what the rest of the module calls a file.
    #[test]
    fn a_renames_status_is_read_from_its_new_path() {
        let kinds = name_status("R085\0old.rs\0new.rs\0A\0fresh.rs\0D\0gone.rs\0");

        assert_eq!(kinds.get("new.rs"), Some(&ChangeKind::Renamed));
        assert_eq!(kinds.get("fresh.rs"), Some(&ChangeKind::Added));
        assert_eq!(kinds.get("gone.rs"), Some(&ChangeKind::Deleted));
    }

    /// An uneven replace is the case a browser-side pairing gets wrong: three
    /// lines becoming one leaves two rows with nothing opposite them.
    #[test]
    fn an_uneven_replacement_leaves_single_sided_rows() {
        let (hunks, verdict) = pair("one\ntwo\nthree\n", "only\n");
        assert_eq!(verdict, DiffVerdict::Shown);

        let rows: Vec<&DiffRow> = hunks.iter().flat_map(|h| &h.rows).collect();
        assert_eq!(rows.len(), 3);
        assert!(rows[0].old.is_some() && rows[0].new.is_some(), "the pair");
        assert!(
            rows[1..].iter().all(|r| r.old.is_some() && r.new.is_none()),
            "the leftovers stand alone rather than being paired with nothing"
        );
    }

    /// A wholesale rewrite is cut off, and says how much was dropped -- the
    /// King is told the panel is partial rather than being shown a browser that
    /// has stopped responding.
    #[test]
    fn an_enormous_rewrite_is_truncated_and_says_so() {
        let before: String = (0..MOST_ROWS + 500).map(|i| format!("old {i}\n")).collect();
        let after: String = (0..MOST_ROWS + 500).map(|i| format!("new {i}\n")).collect();

        let (hunks, verdict) = pair(&before, &after);
        let rows: usize = hunks.iter().map(|h| h.rows.len()).sum();

        assert!(rows <= MOST_ROWS, "{rows} rows drawn");
        assert!(
            matches!(verdict, DiffVerdict::Truncated(dropped) if dropped > 0),
            "{verdict:?}"
        );
    }

    /// git counts a file with no trailing newline as ending a line anyway, and
    /// an untracked file's count has to agree with the tracked ones beside it.
    #[test]
    fn lines_are_counted_as_git_counts_them() {
        assert_eq!(count_lines(b""), 0);
        assert_eq!(count_lines(b"one\n"), 1);
        assert_eq!(count_lines(b"one\ntwo"), 2);
        assert_eq!(count_lines(b"one\ntwo\n"), 2);
    }

    // -- Reading one file whole ----------------------------------------------

    /// Numbering is 1-based, and a trailing newline does not invent a last
    /// empty line.
    ///
    /// The numbering is the load-bearing half: the number rendered in the
    /// gutter is the number a note against that line reports back to the court,
    /// so an off-by-one here would send the model an objection about the line
    /// above the one the King read.
    #[tokio::test]
    async fn a_file_is_numbered_as_an_editor_numbers_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lex.rs"), "one\ntwo\nthree\n").unwrap();

        let read = source(&workspace(dir.path()), "lex.rs").await;

        assert_eq!(read.verdict, DiffVerdict::Shown);
        assert_eq!(read.language, Language::Rust);
        assert_eq!(
            read.lines.iter().map(|l| l.number).collect::<Vec<_>>(),
            vec![1, 2, 3],
            "a trailing newline ends the last line rather than opening another"
        );
        assert_eq!(read.lines[0].text, "one");
        assert_eq!(read.lines[2].text, "three");

        // And with no trailing newline, the last line is still a line.
        std::fs::write(dir.path().join("lex.rs"), "one\ntwo").unwrap();
        let read = source(&workspace(dir.path()), "lex.rs").await;
        assert_eq!(read.lines.len(), 2);
    }

    /// The three things that are not a file to read, each named rather than
    /// rendered as an empty document.
    ///
    /// An empty answer is ambiguous -- an empty file, a binary one and a missing
    /// one would all draw identically -- which is the same objection
    /// `ChangeSummary::note` exists to answer.
    #[tokio::test]
    async fn what_cannot_be_read_says_which_rather_than_rendering_empty() {
        let dir = tempfile::tempdir().unwrap();
        let space = workspace(dir.path());

        std::fs::write(dir.path().join("seal.png"), [0x89, 0x50, 0x00, 0x01]).unwrap();
        let binary = source(&space, "seal.png").await;
        assert_eq!(binary.verdict, DiffVerdict::Binary);
        assert!(binary.lines.is_empty());

        let missing = source(&space, "never.rs").await;
        assert!(
            matches!(missing.verdict, DiffVerdict::Unreadable(_)),
            "{:?}",
            missing.verdict
        );

        // A folder is the ordinary mistake: the tree lists both kinds of row.
        std::fs::create_dir(dir.path().join("src")).unwrap();
        let folder = source(&space, "src").await;
        assert!(
            matches!(&folder.verdict, DiffVerdict::Unreadable(why) if why.contains("folder")),
            "{:?}",
            folder.verdict
        );

        // An empty file is a real answer and must NOT be confused with any of
        // the above: no lines, but shown.
        std::fs::write(dir.path().join("empty.rs"), "").unwrap();
        let empty = source(&space, "empty.rs").await;
        assert_eq!(empty.verdict, DiffVerdict::Shown);
        assert!(empty.lines.is_empty());
    }

    /// A file too large to send is refused before it is read, and one too long
    /// to draw is cut off and says by how much.
    ///
    /// Both share the diff's own thresholds: the browser's limit is the same
    /// limit whichever panel is rendering into it.
    #[tokio::test]
    async fn an_oversized_file_is_refused_and_a_long_one_is_truncated() {
        let dir = tempfile::tempdir().unwrap();
        let space = workspace(dir.path());

        let huge = "x".repeat(MOST_BYTES as usize + 1);
        std::fs::write(dir.path().join("bundle.js"), &huge).unwrap();
        assert_eq!(
            source(&space, "bundle.js").await.verdict,
            DiffVerdict::TooLarge
        );

        let long: String = (0..MOST_ROWS + 40).map(|i| format!("line {i}\n")).collect();
        std::fs::write(dir.path().join("long.rs"), &long).unwrap();
        let read = source(&space, "long.rs").await;

        assert_eq!(read.lines.len(), MOST_ROWS);
        assert_eq!(read.verdict, DiffVerdict::Truncated(40));
        assert_eq!(
            read.lines.last().unwrap().number,
            MOST_ROWS as u32,
            "a truncated file still numbers the lines it did draw honestly"
        );
    }
}
