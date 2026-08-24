//! Preparing a place on disk for a plan to work, and disposing of it after.
//!
//! Server-only. Everything here shells out to `git` rather than linking a git
//! library: the operations are few, the CLI's behaviour is the contract users
//! already know, and its refusals are written in words worth showing the King
//! verbatim.
//!
//! A worktree is disposable exactly when its work has been dealt with -- landed
//! on the branch it was cut from, or preserved somewhere it can be recovered
//! from. Until then it persists under `.kingdom/`, because guessing at that
//! would throw away real work. [`merge`] and [`archive`] are the two answers.

use kingdom_core::{Outcome, Workspace, WorkspaceMode};
use std::path::{Path, PathBuf};

/// Folder inside a city where isolated worktrees are cut.
pub const WORKTREE_DIR: &str = ".kingdom";

/// Prefix for branches Kingdom creates, so they are obvious in `git branch`.
const BRANCH_PREFIX: &str = "kingdom/";

#[derive(Debug, thiserror::Error)]
pub enum WorktreeError {
    #[error(
        "{0} is not a git repository, so it cannot be worked in a worktree. \
         Choose \"This folder\" instead, or run `git init` there."
    )]
    NotARepo(String),
    #[error("could not run git: {0}")]
    Spawn(String),
    /// git's own refusal, passed through untouched -- it is nearly always more
    /// specific than anything this layer could say about it.
    #[error("git refused: {0}")]
    Git(String),
}

/// Prepares the workspace a decree asked for.
///
/// `city_root` is the absolute path to the project. The returned [`Workspace`]
/// is what the plan records and what every later read is pointed at.
pub async fn prepare(city_root: &Path, mode: &WorkspaceMode) -> Result<Workspace, WorktreeError> {
    if let WorkspaceMode::InPlace = mode {
        return Ok(Workspace::in_place(city_root.to_string_lossy()));
    }

    if !city_root.join(".git").exists() {
        return Err(WorktreeError::NotARepo(
            city_root
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("This project")
                .to_string(),
        ));
    }

    // Kingdom's own scratch space must not show up as uncommitted work in the
    // repo it is isolating. `info/exclude` rather than `.gitignore`, because the
    // King's repository is not ours to commit to.
    exclude_worktree_dir(city_root);

    // Recorded now, while it is unambiguously true. Asking at merge time would
    // land the work wherever the King had wandered to since.
    let base = git(city_root, &["rev-parse", "--abbrev-ref", "HEAD"])
        .await
        .ok()
        .map(|b| b.trim().to_string())
        .filter(|b| !b.is_empty() && b != "HEAD");

    let id = uuid::Uuid::new_v4().to_string();
    let dir: PathBuf = city_root.join(WORKTREE_DIR).join(&id);
    let dir_arg = dir.to_string_lossy().to_string();

    let branch = match mode {
        // A fresh worktree gets a branch of its own, so committing in it cannot
        // move a branch the King is using elsewhere.
        WorkspaceMode::Fresh => {
            let branch = format!("{BRANCH_PREFIX}{id}");
            git(
                city_root,
                &["worktree", "add", "-b", &branch, &dir_arg, "HEAD"],
            )
            .await?;
            branch
        }
        // An existing branch is checked out as-is. git refuses if it is already
        // checked out in another worktree, and that refusal is the right answer:
        // two agents on one branch is the collision this feature exists to stop.
        WorkspaceMode::Branch(b) => {
            git(city_root, &["worktree", "add", &dir_arg, b]).await?;
            b.clone()
        }
        WorkspaceMode::InPlace => unreachable!("handled above"),
    };

    Ok(Workspace {
        mode: mode.clone(),
        path: dir_arg,
        branch: Some(branch),
        id: Some(id),
        base,
    })
}

/// What became of an attempt to finish a plan.
///
/// A refusal is **not** an `Err`. git declining to merge -- a conflict, a dirty
/// tree, the wrong branch checked out -- is a real event in the plan's life and
/// belongs in the plan's log, where the King is already looking. `Err` is kept
/// for the server being unable to do the work at all.
pub enum Finish {
    /// It is done. The plan settles with this outcome.
    Settled(Outcome),
    /// It could not be done, in words worth showing verbatim. The plan keeps
    /// whatever status it had, because nothing about it has changed.
    Refused(String),
}

/// Lands a plan's work on the branch its workspace was cut from.
///
/// Every failure stops the whole thing and leaves the city exactly as it was:
/// a refused merge is aborted, so there is never a half-merged working tree for
/// the King to discover later.
pub async fn merge(city_root: &Path, workspace: &Workspace) -> Result<Finish, WorktreeError> {
    // Working in place, there is nothing to merge -- the work is already in the
    // folder. Saying so plainly beats pretending to merge (a lie) or refusing
    // (a plan the King cannot close).
    let Some(branch) = workspace
        .branch
        .as_deref()
        .filter(|_| workspace.is_isolated())
    else {
        return Ok(Finish::Settled(Outcome::Merged {
            commit: head_of(city_root).await.unwrap_or_default(),
            into: "this folder".to_string(),
        }));
    };

    let Some(base) = workspace.base.as_deref() else {
        return Ok(Finish::Refused(
            "This plan does not record which branch it was cut from, so there is \
             nowhere certain to merge it. Merge it by hand."
                .to_string(),
        ));
    };

    let worktree = PathBuf::from(&workspace.path);
    commit_pending(&worktree, "Kingdom: work in progress").await?;

    // Merging requires the base branch to be the one checked out here. Checking
    // it out ourselves would move the King's working copy out from under him --
    // precisely the collision this product exists to prevent.
    let on = git(city_root, &["rev-parse", "--abbrev-ref", "HEAD"])
        .await?
        .trim()
        .to_string();
    if on != base {
        return Ok(Finish::Refused(format!(
            "This plan was cut from {base}, but {} has {on} checked out. \
             Switch back to {base} and try again.",
            city_root.file_name().unwrap_or_default().to_string_lossy(),
        )));
    }

    // `--no-ff` because a plan is the unit of review: one merge commit per plan
    // is what makes "what did that plan actually land?" answerable afterwards.
    let message = format!("Merge {branch}");
    let merged = git(city_root, &["merge", "--no-ff", "-m", &message, branch]).await;

    if let Err(e) = merged {
        // Leave the city exactly as it was found. A half-merged tree the King
        // discovers hours later is far worse than a refusal he reads now.
        let conflicts = git(city_root, &["diff", "--name-only", "--diff-filter=U"])
            .await
            .unwrap_or_default();
        let _ = git(city_root, &["merge", "--abort"]).await;

        let mut refusal = e.to_string();
        let paths: Vec<&str> = conflicts
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        if !paths.is_empty() {
            refusal.push_str("\n\nConflicting files:\n");
            for path in paths {
                refusal.push_str(&format!("  {path}\n"));
            }
        }
        return Ok(Finish::Refused(refusal));
    }

    let commit = head_of(city_root).await?;

    // The work has landed, so the checkout is now genuinely disposable. `-d`
    // rather than `-D`: the safe delete, which succeeds only *because* the
    // branch is merged, so a bug here refuses rather than destroys.
    git(
        city_root,
        &["worktree", "remove", "--force", &workspace.path],
    )
    .await?;
    git(city_root, &["branch", "-d", branch]).await?;

    Ok(Finish::Settled(Outcome::Merged {
        commit,
        into: base.to_string(),
    }))
}

/// Sets a plan's work aside, preserved, and reclaims its checkout.
///
/// The promise is that the checkout goes and the work does not: a patch of the
/// branch is written to `patch_path`, replayable with `git am`, and it is that
/// patch -- not the branch -- that is the record. Once it is safely on disk the
/// branch goes too, so archiving a dozen plans does not leave a dozen
/// `kingdom/<uuid>` entries in the King's `git branch`.
pub async fn archive(
    city_root: &Path,
    workspace: &Workspace,
    patch_path: &Path,
) -> Result<Finish, WorktreeError> {
    // Nothing was isolated, so there is no checkout to reclaim and no branch to
    // keep. Archiving is then purely a matter of record.
    let (Some(branch), Some(base)) = (workspace.branch.as_deref(), workspace.base.as_deref())
    else {
        let tip = head_of(city_root).await.unwrap_or_default();
        return Ok(Finish::Settled(Outcome::Archived {
            branch: workspace.branch.clone().unwrap_or_else(|| "none".into()),
            base_commit: tip.clone(),
            tip,
            base: workspace.base.clone().unwrap_or_else(|| "none".into()),
            patch: None,
            pruned: false,
        }));
    };

    let worktree = PathBuf::from(&workspace.path);
    // Committed first, or `worktree remove --force` would throw the work away --
    // which would make archiving a destructive act wearing a gentle name.
    commit_pending(&worktree, "Kingdom: archived work in progress").await?;

    let tip = head_of(&worktree).await?;

    // The branch `base` names will have moved by the time anyone restores, so
    // the sha is recorded now -- it is what the patch was cut from, and what a
    // `git am` would have to sit on top of.
    let base_commit = git(city_root, &["rev-parse", base])
        .await
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    // `format-patch` rather than `diff`: it keeps each commit's message and
    // author and replays with `git am`. That is the difference between
    // recovering a change and recovering the work. `--binary` because without
    // it git writes "Binary files differ", which restores nothing.
    let range = format!("{base}..{branch}");
    let patch = match git(city_root, &["format-patch", "--binary", "--stdout", &range]).await {
        Ok(body) if !body.trim().is_empty() => {
            let written = patch_path
                .parent()
                .is_none_or(|d| std::fs::create_dir_all(d).is_ok())
                && std::fs::write(patch_path, &body).is_ok();
            written.then(|| patch_path.to_string_lossy().to_string())
        }
        // A plan that changed nothing has no patch, and that is not a failure.
        // Nor is an unwritable path -- but in both cases the branch is then the
        // only record there is, and must be left alone.
        _ => None,
    };

    git(
        city_root,
        &["worktree", "remove", "--force", &workspace.path],
    )
    .await?;

    // Two conditions, both load-bearing. Without a patch the branch is the only
    // copy of the work. And a branch Kingdom did not create is the King's own:
    // deleting that would be destroying his work, not tidying up after ours.
    let pruned = if patch.is_some() && branch.starts_with(BRANCH_PREFIX) {
        // `-D`, because the branch was never merged -- that is the whole point.
        // A failure here is untidiness, not lost work, so it does not fail the
        // archive; the branch simply stays, and the outcome says so.
        git(city_root, &["branch", "-D", branch]).await.is_ok()
    } else {
        false
    };

    Ok(Finish::Settled(Outcome::Archived {
        branch: branch.to_string(),
        tip,
        base: base.to_string(),
        base_commit,
        patch,
        pruned,
    }))
}

/// Commits whatever is uncommitted in a worktree, if anything is.
///
/// Both endings do this first. Refusing until the King tidies up would strand
/// work behind a UI that offers no way to tidy; a commit on a throwaway branch
/// is fully reversible, and a discarded edit is not.
async fn commit_pending(worktree: &Path, message: &str) -> Result<bool, WorktreeError> {
    let dirty = git(worktree, &["status", "--porcelain"]).await?;
    if dirty.trim().is_empty() {
        return Ok(false);
    }

    git(worktree, &["add", "-A"]).await?;
    git(worktree, &["commit", "-m", message]).await?;
    Ok(true)
}

/// The commit a repository's HEAD points at.
async fn head_of(repo: &Path) -> Result<String, WorktreeError> {
    Ok(git(repo, &["rev-parse", "HEAD"]).await?.trim().to_string())
}

/// Local branches in a repository, HEAD's branch first.
///
/// Offered to the picker so the King chooses a branch that exists rather than
/// typing one that does not. A non-git or unreadable folder yields an empty list
/// rather than an error: the picker simply has nothing to offer, which is true.
pub async fn branches(city_root: &Path) -> Vec<String> {
    if !city_root.join(".git").exists() {
        return Vec::new();
    }

    let Ok(out) = git(
        city_root,
        &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
    )
    .await
    else {
        return Vec::new();
    };

    let mut names: Vec<String> = out
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        // Branches Kingdom cut for its own worktrees are noise in a list whose
        // whole purpose is picking work the King already has in flight.
        .filter(|l| !l.starts_with(BRANCH_PREFIX))
        .map(str::to_string)
        .collect();

    if let Ok(head) = git(city_root, &["rev-parse", "--abbrev-ref", "HEAD"]).await {
        let head = head.trim().to_string();
        if let Some(i) = names.iter().position(|n| n == &head) {
            names.remove(i);
            names.insert(0, head);
        }
    }

    names
}

/// Adds `.kingdom/` to the repository's local excludes, once.
///
/// Best-effort: a repo whose `info/exclude` cannot be written still works, it
/// just shows Kingdom's scratch folder as untracked. That is cosmetic, and not a
/// reason to refuse the King a workspace.
fn exclude_worktree_dir(city_root: &Path) {
    let entry = format!("{WORKTREE_DIR}/");
    let exclude = city_root.join(".git").join("info").join("exclude");

    let previous = std::fs::read_to_string(&exclude).unwrap_or_default();
    if previous.lines().any(|l| l.trim() == entry) {
        return;
    }

    let _ = std::fs::create_dir_all(exclude.parent().unwrap_or(city_root));
    let separator = if previous.is_empty() || previous.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    let _ = std::fs::write(
        &exclude,
        format!("{previous}{separator}# Kingdom IDE worktrees\n{entry}\n"),
    );
}

/// Runs one git command in a repository, returning its stdout.
async fn git(cwd: &Path, args: &[&str]) -> Result<String, WorktreeError> {
    let output = tokio::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .await
        .map_err(|e| WorktreeError::Spawn(e.to_string()))?;

    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(WorktreeError::Git(if message.is_empty() {
            format!("`git {}` failed", args.join(" "))
        } else {
            message
        }));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        for args in [
            vec!["init", "-b", "main"],
            vec!["config", "user.email", "king@kingdom.test"],
            vec!["config", "user.name", "The King"],
        ] {
            git(root, &args).await.expect("git setup");
        }
        std::fs::write(root.join("README.md"), "hello").unwrap();
        git(root, &["add", "."]).await.unwrap();
        git(root, &["commit", "-m", "first"]).await.unwrap();
        dir
    }

    /// The whole point of the feature: each mode must put the plan somewhere
    /// that really is what it claims to be, and two isolated plans must never
    /// land in the same directory. The King's trust that an agent is fenced in
    /// rests on this being true on disk rather than merely in the type.
    #[tokio::test]
    async fn each_mode_prepares_the_checkout_it_promises() {
        let dir = repo().await;
        let root = dir.path();
        let head = git(root, &["rev-parse", "HEAD"]).await.unwrap();

        // In place: the city itself, and nothing created.
        let here = prepare(root, &WorkspaceMode::InPlace).await.unwrap();
        assert_eq!(here.path, root.to_string_lossy());
        assert!(!here.is_isolated());
        assert!(
            !root.join(WORKTREE_DIR).exists(),
            "working in place must cut nothing"
        );

        // Fresh: its own directory, its own branch, the same commit.
        let fresh = prepare(root, &WorkspaceMode::Fresh).await.unwrap();
        let fresh_path = PathBuf::from(&fresh.path);
        assert!(
            fresh_path.join("README.md").is_file(),
            "a fresh workspace must be a real checkout"
        );
        assert_eq!(
            git(&fresh_path, &["rev-parse", "HEAD"]).await.unwrap(),
            head,
            "a fresh worktree is cut from the city's current HEAD"
        );
        assert_ne!(
            fresh.branch.as_deref(),
            Some("main"),
            "a fresh worktree must not move a branch the King is using"
        );

        // A second fresh worktree must not collide with the first.
        let other = prepare(root, &WorkspaceMode::Fresh).await.unwrap();
        assert_ne!(
            fresh.path, other.path,
            "two plans must not share a checkout"
        );

        // Branch: that branch, checked out.
        git(root, &["branch", "fix/parser"]).await.unwrap();
        let named = prepare(root, &WorkspaceMode::Branch("fix/parser".into()))
            .await
            .unwrap();
        assert_eq!(
            git(
                &PathBuf::from(&named.path),
                &["rev-parse", "--abbrev-ref", "HEAD"]
            )
            .await
            .unwrap()
            .trim(),
            "fix/parser"
        );

        // Where it was cut from, recorded while it is unambiguously true. This
        // is what a later merge trusts instead of asking the city's HEAD, which
        // by then may have moved.
        assert_eq!(
            fresh.base.as_deref(),
            Some("main"),
            "a workspace records the branch it was cut from"
        );

        // Isolation must not itself dirty the repo it is isolating.
        assert_eq!(
            git(root, &["status", "--porcelain"]).await.unwrap().trim(),
            "",
            "cutting worktrees must leave the city clean"
        );
    }

    /// The King asked to be fenced in. Quietly handing him the live folder
    /// instead would be the exact failure this feature exists to prevent, so a
    /// non-git city must refuse rather than downgrade.
    #[tokio::test]
    async fn a_city_without_git_refuses_isolation_rather_than_downgrading() {
        let dir = tempfile::tempdir().unwrap();
        let outcome = prepare(dir.path(), &WorkspaceMode::Fresh).await;
        assert!(matches!(outcome, Err(WorktreeError::NotARepo(_))));
    }

    /// Writes a file in a repository and commits it.
    async fn commit(repo: &Path, name: &str, body: &str) {
        std::fs::write(repo.join(name), body).unwrap();
        git(repo, &["add", "."]).await.unwrap();
        git(repo, &["commit", "-m", &format!("edit {name}")])
            .await
            .unwrap();
    }

    /// The invariant the whole feature rests on: a merge that cannot be done
    /// must leave the King's project exactly as it was found.
    ///
    /// A half-merged working tree discovered hours later is far worse than a
    /// refusal read now -- the King would have no way of telling which of the
    /// files in front of him were his own work and which an agent's. So the
    /// abort matters as much as the refusal, and both are pinned here along
    /// with the worktree surviving, so the work is still there to retry with.
    #[tokio::test]
    async fn a_conflicted_merge_leaves_the_city_exactly_as_it_was() {
        let dir = repo().await;
        let root = dir.path();

        let workspace = prepare(root, &WorkspaceMode::Fresh).await.unwrap();
        let worktree = PathBuf::from(&workspace.path);

        // The same file, edited two different ways.
        commit(&worktree, "README.md", "the plan's version").await;
        commit(root, "README.md", "the King's version").await;

        let before = head_of(root).await.unwrap();

        let finish = merge(root, &workspace).await.unwrap();
        let Finish::Refused(why) = finish else {
            panic!("a conflicting merge must be refused, not performed");
        };
        assert!(
            why.contains("README.md"),
            "the refusal must name the conflicting file; got: {why}"
        );

        assert_eq!(
            head_of(root).await.unwrap(),
            before,
            "a refused merge must not move the city's HEAD"
        );
        assert_eq!(
            git(root, &["status", "--porcelain"]).await.unwrap().trim(),
            "",
            "a refused merge must leave no conflict markers behind"
        );
        assert!(
            !root.join(".git").join("MERGE_HEAD").exists(),
            "a refused merge must be aborted, not left half-done"
        );
        assert!(
            worktree.join("README.md").is_file(),
            "the plan's work must survive a refusal, so it can be retried"
        );
    }

    /// The happy path, and the answer to the question this module deferred:
    /// a worktree is disposable exactly when its work has landed.
    #[tokio::test]
    async fn a_clean_merge_lands_the_work_and_disposes_of_the_worktree() {
        let dir = repo().await;
        let root = dir.path();

        let workspace = prepare(root, &WorkspaceMode::Fresh).await.unwrap();
        let worktree = PathBuf::from(&workspace.path);
        let branch = workspace.branch.clone().unwrap();

        // Uncommitted on purpose: the King should not have to tidy up before he
        // can finish, and a UI that offers no way to tidy would strand the work.
        std::fs::write(worktree.join("tower.rs"), "fn build() {}").unwrap();

        let finish = merge(root, &workspace).await.unwrap();
        let Finish::Settled(Outcome::Merged { commit, into }) = finish else {
            panic!("a clean merge must settle as merged");
        };

        assert_eq!(into, "main");
        assert_eq!(commit, head_of(root).await.unwrap());
        assert!(
            root.join("tower.rs").is_file(),
            "the work must actually be in the project afterwards"
        );
        assert!(
            !worktree.exists(),
            "a landed worktree is disposable, and must be disposed of"
        );
        assert!(
            !branches(root).await.contains(&branch),
            "the plan's branch goes with its worktree once merged"
        );
    }

    /// Archiving's whole promise: the checkout goes, the work does not.
    ///
    /// "This didn't work out" must never mean "and so it was deleted". The
    /// branch is reclaimed along with the checkout -- but only because the patch
    /// has been written first, so what is checked here is that the patch really
    /// does carry the work it is now solely responsible for.
    #[tokio::test]
    async fn archiving_keeps_the_work_recoverable() {
        let dir = repo().await;
        let root = dir.path();

        let workspace = prepare(root, &WorkspaceMode::Fresh).await.unwrap();
        let worktree = PathBuf::from(&workspace.path);
        std::fs::write(worktree.join("folly.rs"), "fn doomed() {}").unwrap();

        let patch_path = dir.path().join("archive").join("plan-1.patch");
        let finish = archive(root, &workspace, &patch_path).await.unwrap();
        let Finish::Settled(Outcome::Archived {
            branch,
            tip,
            base_commit,
            patch,
            pruned,
            ..
        }) = finish
        else {
            panic!("archiving must settle as archived");
        };

        assert!(
            !worktree.exists(),
            "reclaiming the checkout is the point of archiving"
        );
        assert!(pruned, "the plan's branch goes with its worktree");
        assert!(
            !branches(root).await.contains(&branch),
            "an archived plan must not leave its branch cluttering the city"
        );
        assert!(
            !root.join("folly.rs").exists(),
            "archived work must not land in the project"
        );

        // The shas are the only remaining coordinates of the work, so they have
        // to be real ones: `tip` for `git show`, `base_commit` for `git am` to
        // sit on top of.
        for sha in [&tip, &base_commit] {
            assert!(
                git(root, &["cat-file", "-e", sha]).await.is_ok(),
                "the outcome must record a commit that exists; got: {sha}"
            );
        }

        let patch = patch.expect("a plan that changed something has a patch");
        let body = std::fs::read_to_string(&patch).expect("the patch is on disk");
        assert!(
            body.contains("folly.rs") && body.contains("fn doomed()"),
            "the patch must carry the work, or it recovers nothing"
        );
    }

    /// The destructive edge of pruning: a plan working on a branch the King
    /// named is working on *his* branch, which almost certainly has a life
    /// beyond this plan. Tidying up after ourselves must never reach that far.
    #[tokio::test]
    async fn archiving_never_deletes_a_branch_the_king_named() {
        let dir = repo().await;
        let root = dir.path();

        git(root, &["branch", "fix/parser"]).await.unwrap();
        let workspace = prepare(root, &WorkspaceMode::Branch("fix/parser".into()))
            .await
            .unwrap();
        let worktree = PathBuf::from(&workspace.path);
        std::fs::write(worktree.join("parser.rs"), "fn parse() {}").unwrap();

        let patch_path = dir.path().join("archive").join("plan-2.patch");
        let finish = archive(root, &workspace, &patch_path).await.unwrap();
        let Finish::Settled(Outcome::Archived { pruned, .. }) = finish else {
            panic!("archiving must settle as archived");
        };

        assert!(!pruned, "the King's own branch is not ours to reclaim");
        assert!(
            branches(root).await.contains(&"fix/parser".to_string()),
            "archiving must leave a branch the King named exactly where it was"
        );
    }
}
