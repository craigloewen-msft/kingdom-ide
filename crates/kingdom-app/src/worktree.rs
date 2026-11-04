//! Preparing a place on disk for a plan to work, and disposing of it after.
//!
//! Server-only. Everything here shells out to `git` rather than linking a git
//! library: the operations are few, the CLI's behaviour is the contract users
//! already know, and its refusals are written in words worth showing the user
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

/// Prepares the workspace a prompt asked for.
///
/// `city_root` is the absolute path to the project. `slug` is the plan's name in
/// git-safe form ([`kingdom_core::naming::slugify`]); a fresh workspace's branch
/// is cut from it. The returned [`Workspace`] is what the plan records and what
/// every later read is pointed at.
pub async fn prepare(
    city_root: &Path,
    mode: &WorkspaceMode,
    slug: &str,
) -> Result<Workspace, WorktreeError> {
    if let WorkspaceMode::InPlace = mode {
        // No worktree to cut, but the workspace still gets a `.kingdom/` of its
        // own -- the artifacts a browser tool leaves for the King live there
        // (`tools::ARTIFACT_DIR`). In place, that directory is inside the
        // *user's own* project folder, so without this his `git status` sprouts
        // untracked files nobody asked for and `git add -A` would commit a
        // screenshot into his history. Same rule, same file, same best-effort
        // terms as the isolated path below.
        exclude_worktree_dir(city_root);
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
    // user's repository is not ours to commit to.
    exclude_worktree_dir(city_root);

    // Recorded now, while it is unambiguously true. Asking at merge time would
    // land the work wherever the user had wandered to since.
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
        // move a branch the user is using elsewhere.
        WorkspaceMode::Fresh => fresh_branch(city_root, slug, &id, &dir_arg).await?,
        // An existing branch is checked out as-is. git refuses if it is already
        // checked out in another worktree, and that refusal is the right answer:
        // two agents on one branch is the collision this feature exists to stop.
        WorkspaceMode::Branch(b) => {
            git(city_root, &["worktree", "add", &dir_arg, b]).await?;
            b.clone()
        }
        WorkspaceMode::PullRequest(pr) => {
            let branch = review_branch(city_root, pr, &id, &dir_arg).await?;
            // The commit actually checked out, which is what the review is of.
            // Usually the one the King was shown; if somebody pushed in
            // between, it is the newer one, and the record says so.
            let mut pr = pr.clone();
            if let Ok(tip) = head_of(&dir).await {
                pr.head_sha = tip;
            }
            // Measured against the branch the pull request asks to land on,
            // not whatever the King happens to have checked out: that is what
            // the review is *of*. Recorded for the changes drawer; nothing is
            // ever merged into it from here -- see `merge`.
            return Ok(Workspace {
                mode: WorkspaceMode::PullRequest(pr.clone()),
                path: dir_arg,
                branch: Some(branch),
                id: Some(id),
                base: Some(pr.base.clone()),
            });
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

/// How many `-2`, `-3`, ... suffixes to try before falling back to the uuid.
///
/// Small on purpose. Past a handful of identically-named plans the number has
/// stopped being informative anyway, and the point of the bound is that a
/// prompt can never be refused merely for resembling an earlier one.
const MAX_ATTEMPTS: u32 = 20;

/// Cuts a fresh worktree on a branch named after the plan.
///
/// Names collide routinely -- "fix the tests" twice in one afternoon is the
/// ordinary case, not the exotic one -- so a taken name is walked past rather
/// than reported. The uuid is kept as the last resort: an ugly branch name is a
/// far better outcome than a refused prompt, and it is also what happens for a
/// prompt with no ASCII in it at all, where every plan would otherwise be
/// fighting over `kingdom/plan`.
async fn fresh_branch(
    city_root: &Path,
    slug: &str,
    id: &str,
    dir_arg: &str,
) -> Result<String, WorktreeError> {
    fresh_branch_at(city_root, slug, id, dir_arg, "HEAD").await
}

/// [`fresh_branch`], cut from any commit rather than only `HEAD`.
async fn fresh_branch_at(
    city_root: &Path,
    slug: &str,
    id: &str,
    dir_arg: &str,
    from: &str,
) -> Result<String, WorktreeError> {
    let taken = branches(city_root).await;

    let candidates = (1..=MAX_ATTEMPTS)
        .map(|n| match n {
            1 => format!("{BRANCH_PREFIX}{slug}"),
            n => format!("{BRANCH_PREFIX}{slug}-{n}"),
        })
        // Asking git first keeps the common case to a single `worktree add`.
        // It is not a guarantee -- another plan could take the name between the
        // listing and the add -- which is why the add's own refusal below is
        // still what actually decides.
        .filter(|b| !taken.contains(b))
        .chain(std::iter::once(format!("{BRANCH_PREFIX}{id}")));

    let mut last = None;
    for branch in candidates {
        let args = ["worktree", "add", "-b", &branch, dir_arg, from];
        match git(city_root, &args).await {
            Ok(_) => return Ok(branch),
            // Only a name clash is worth another attempt. Anything else -- a
            // repo we cannot write, a missing HEAD -- would fail identically for
            // every candidate, so retrying would only bury git's own words under
            // twenty repetitions of them.
            Err(e) if is_name_clash(&e) => last = Some(e),
            Err(e) => return Err(e),
        }
    }

    Err(last.unwrap_or_else(|| {
        WorktreeError::Git(format!("could not find a free branch name for {slug}"))
    }))
}

/// Whether git refused because the branch name is already taken.
///
/// Matched on the message because git offers no distinct exit code for it. The
/// cost of a wrong match is bounded both ways: a false positive tries the next
/// name and reports the same error if that fails too, and a false negative
/// surfaces git's refusal verbatim, which is what would have happened anyway.
fn is_name_clash(e: &WorktreeError) -> bool {
    let WorktreeError::Git(message) = e else {
        return false;
    };
    let message = message.to_lowercase();
    message.contains("already exists") || message.contains("already used by worktree")
}

/// Fetches a pull request and checks it out into a worktree of its own.
///
/// # Why a ref of our own rather than `FETCH_HEAD`
///
/// `FETCH_HEAD` is one file per repository. Two reviews starting at once --
/// the ordinary case for a King with a queue of pull requests -- would each
/// fetch into it and each check out whichever landed last. A uniquely named
/// ref under `refs/kingdom/` cannot be raced, and it is deleted the moment the
/// worktree's own branch holds the commit.
///
/// `pull/N/head` lives on the *base* repository, so a pull request from a fork
/// checks out exactly the same way and needs no remote for the fork.
///
/// The base branch is fetched too, best-effort, so the changes drawer can diff
/// against the remote's view of it -- which is what GitHub shows -- rather
/// than against a local copy that may be days stale.
async fn review_branch(
    city_root: &Path,
    pr: &kingdom_core::PullRequestRef,
    id: &str,
    dir_arg: &str,
) -> Result<String, WorktreeError> {
    let scratch = format!("refs/kingdom/fetch/{id}");
    let spec = format!("+pull/{}/head:{scratch}", pr.number);
    git(city_root, &["fetch", "--no-tags", &pr.remote, &spec]).await?;
    let _ = git(city_root, &["fetch", "--no-tags", &pr.remote, &pr.base]).await;

    // If somebody pushed since the King looked, the fetched tip wins: the
    // review is of what is actually there, and `prepare` records it.
    let slug = format!("pr-{}-review", pr.number);
    let result = fresh_branch_at(city_root, &slug, id, dir_arg, &scratch).await;
    let _ = git(city_root, &["update-ref", "-d", &scratch]).await;
    result
}

/// What became of an attempt to finish a plan.
///
/// A refusal is **not** an `Err`. git declining to merge -- a conflict, a dirty
/// tree, the wrong branch checked out -- is a real event in the plan's life and
/// belongs in the plan's log, where the user is already looking. `Err` is kept
/// for the server being unable to do the work at all.
///
/// The two refusals are split by **whose move it is next**, which is the only
/// question the conversation asks of them. Kingdom declining to try is the
/// user's to settle; git trying and failing is work, and work is what an agent
/// is for.
pub enum Finish {
    /// It is done. The plan settles with this outcome.
    Settled(Outcome),
    /// It could not be done, in words worth showing verbatim. The plan keeps
    /// whatever status it had, because nothing about it has changed.
    Refused(String),
    /// git attempted the merge and declined: the branch has diverged from its
    /// base. Like [`Finish::Refused`] in every respect that touches disk --
    /// the merge is aborted and the plan keeps its status -- and different in
    /// exactly one: this one names a job, so the chamber can offer to send the
    /// court after the latest base branch rather than leaving the user to type
    /// the request himself.
    Diverged(String),
}

/// Lands a plan's work on the branch its workspace was cut from.
///
/// Every failure stops the whole thing and leaves the city exactly as it was:
/// a refused merge is aborted, so there is never a half-merged working tree for
/// the user to discover later.
pub async fn merge(city_root: &Path, workspace: &Workspace) -> Result<Finish, WorktreeError> {
    // A review is never landed by Kingdom. The pull request is somebody's
    // proposal to the project, and merging it is a decision the King makes on
    // GitHub, where the people who wrote it can see it made. The conversation
    // never offers this, but the blast radius is his own branch, so the guard
    // lives here as well.
    if let Some(pr) = workspace.mode.pull_request() {
        return Ok(Finish::Refused(format!(
            "This plan is a review of pull request #{}, and Kingdom does not merge \
             pull requests. Merge it on GitHub if the findings say it is ready, then \
             close this review.",
            pr.number
        )));
    }

    // Working in place, there is nothing to merge -- the work is already in the
    // folder. Saying so plainly beats pretending to merge (a lie) or refusing
    // (a plan the user cannot close).
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
    // it out ourselves would move the user's working copy out from under him --
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
        // Leave the city exactly as it was found. A half-merged tree the user
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
        return Ok(Finish::Diverged(refusal));
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
/// `kingdom/<uuid>` entries in the user's `git branch`.
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
    // copy of the work. And a branch Kingdom did not create is the user's own:
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

/// Closes a review: its checkout and its scratch branch go, and nothing lands.
///
/// The court is told not to edit a pull request it is reviewing, and holds no
/// unrestricted `patch` -- but it does hold `bash`, and a build can leave
/// tracked files changed. Anything it left is kept as a patch at `patch_path`
/// before the worktree is removed, on `archive`'s rule: removing a checkout
/// must never be the thing that destroys work.
///
/// Only a `kingdom/` branch is ever deleted, so a bug here cannot take a branch
/// the King made.
pub async fn close_review(
    city_root: &Path,
    workspace: &Workspace,
    patch_path: &Path,
) -> Result<Finish, WorktreeError> {
    let Some(pr) = workspace.mode.pull_request() else {
        return Ok(Finish::Refused(
            "This plan is not a review, so it cannot be closed as one.".to_string(),
        ));
    };
    let worktree = PathBuf::from(&workspace.path);

    let mut patch = None;
    if worktree.is_dir() {
        // Tracked changes only: untracked files in a review checkout are build
        // output, `node_modules` and the like, and none of it is the court's
        // work worth keeping.
        if let Ok(diff) = git(&worktree, &["diff", "--binary", "HEAD"]).await {
            if !diff.trim().is_empty() {
                let written = patch_path
                    .parent()
                    .is_none_or(|d| std::fs::create_dir_all(d).is_ok())
                    && std::fs::write(patch_path, &diff).is_ok();
                if !written {
                    return Ok(Finish::Refused(format!(
                        "The court left changes in this review's checkout and they could \
                         not be saved to {}, so the checkout was left where it is. Look at \
                         {} before closing it.",
                        patch_path.display(),
                        workspace.path
                    )));
                }
                patch = Some(patch_path.to_string_lossy().to_string());
            }
        }

        git(
            city_root,
            &["worktree", "remove", "--force", &workspace.path],
        )
        .await?;
    } else {
        // Already gone -- removed by hand, perhaps. Tidy git's record of it so
        // the branch below can be deleted.
        let _ = git(city_root, &["worktree", "prune"]).await;
    }

    if let Some(branch) = workspace
        .branch
        .as_deref()
        .filter(|b| b.starts_with(BRANCH_PREFIX))
    {
        // `-D`: a review branch is never merged, by design. Failure is untidy
        // rather than harmful, so it does not fail the close.
        let _ = git(city_root, &["branch", "-D", branch]).await;
    }

    Ok(Finish::Settled(Outcome::Reviewed {
        number: pr.number,
        head: pr.head_sha.clone(),
        patch,
    }))
}

/// Commits whatever is uncommitted in a worktree, if anything is.
///
/// Both endings do this first. Refusing until the user tidies up would strand
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
/// Offered to the picker so the user chooses a branch that exists rather than
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
        // whole purpose is picking work the user already has in flight.
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

/// Excludes Kingdom's scratch folder from the repository, while keeping the
/// one file in it that belongs to the project.
///
/// # Why this is not simply `.kingdom/`
///
/// It was, until a city's shared services got a manifest at
/// `.kingdom/services.toml`. That file is the *project's* -- it says what the
/// project needs standing in order to run -- so it must be committable, while
/// the worktrees beside it must stay hidden.
///
/// **git cannot re-include a file whose parent directory is excluded.** A
/// `.kingdom/` rule excludes the directory itself and git never looks inside,
/// so a later `!.kingdom/services.toml` has nothing to act on. The form that
/// works is to exclude the directory's *contents* -- `.kingdom/*` -- and then
/// negate the one path. Measured against a real repository, not inferred:
/// under the old rule `git status` reported nothing at all.
///
/// Best-effort: a repo whose `info/exclude` cannot be written still works, it
/// just shows Kingdom's scratch folder as untracked. That is cosmetic, and not a
/// reason to refuse the user a workspace.
fn exclude_worktree_dir(city_root: &Path) {
    // A folder that is not a repository has nothing to exclude from, and
    // writing would leave a stray `.git/info/` in somebody's directory. In
    // place, that folder is the user's own, and it may well not be a repo --
    // the isolated path refuses such a city before reaching here, the in-place
    // path does not.
    if !city_root.join(".git").exists() {
        return;
    }

    let contents = format!("{WORKTREE_DIR}/*");
    let keep = format!("!{}", kingdom_core::services::MANIFEST_PATH);
    let exclude = city_root.join(".git").join("info").join("exclude");

    let previous = std::fs::read_to_string(&exclude).unwrap_or_default();
    if previous.lines().any(|l| l.trim() == contents) {
        return;
    }

    // A repository excluded by the *old* rule keeps it forever otherwise, and
    // its manifest stays invisible. Dropped rather than left in place, because
    // `.kingdom/` and `.kingdom/*` do not compose -- the first one still hides
    // the directory and wins.
    let carried: String = previous
        .lines()
        .filter(|line| line.trim() != format!("{WORKTREE_DIR}/"))
        .collect::<Vec<_>>()
        .join("\n");

    let _ = std::fs::create_dir_all(exclude.parent().unwrap_or(city_root));
    let separator = if carried.is_empty() || carried.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    let _ = std::fs::write(
        &exclude,
        format!(
            "{carried}{separator}# Kingdom IDE worktrees, but not the project's \
             own service manifest\n{contents}\n{keep}\n"
        ),
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
    /// land in the same directory. The user's trust that an agent is fenced in
    /// rests on this being true on disk rather than merely in the type.
    #[tokio::test]
    async fn each_mode_prepares_the_checkout_it_promises() {
        let dir = repo().await;
        let root = dir.path();
        let head = git(root, &["rev-parse", "HEAD"]).await.unwrap();

        // In place: the city itself, and nothing created.
        let here = prepare(root, &WorkspaceMode::InPlace, "here")
            .await
            .unwrap();
        assert_eq!(here.path, root.to_string_lossy());
        assert!(!here.is_isolated());
        assert!(
            !root.join(WORKTREE_DIR).exists(),
            "working in place must cut nothing"
        );

        // Fresh: its own directory, its own branch, the same commit.
        let fresh = prepare(root, &WorkspaceMode::Fresh, "tidy-the-sidebar")
            .await
            .unwrap();
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

        assert_eq!(
            fresh.branch.as_deref(),
            Some("kingdom/tidy-the-sidebar"),
            "a plan's branch is named after the plan, not after a uuid"
        );

        // A second fresh worktree must not collide with the first -- and with
        // names rather than uuids, an identical prompt collides by default, so
        // this is the ordinary case rather than a rare one.
        let other = prepare(root, &WorkspaceMode::Fresh, "tidy-the-sidebar")
            .await
            .unwrap();
        assert_ne!(
            fresh.path, other.path,
            "two plans must not share a checkout"
        );
        assert_eq!(
            other.branch.as_deref(),
            Some("kingdom/tidy-the-sidebar-2"),
            "a taken name is walked past, not refused"
        );

        // Branch: that branch, checked out.
        git(root, &["branch", "fix/parser"]).await.unwrap();
        let named = prepare(
            root,
            &WorkspaceMode::Branch("fix/parser".into()),
            "fix-the-parser",
        )
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

    /// The pictures the court takes must be invisible to git, in every mode.
    ///
    /// This is the load-bearing half of keeping a screenshot until the King has
    /// finished with it. A file git reports as untracked sits in the middle of
    /// the plan's own `git status`, and the court cleared exactly such files
    /// away mid-plan -- measured, in four real plans. Worse, `commit_pending`
    /// runs `git add -A` before merging, so anything left would land in the
    /// user's history.
    ///
    /// Asserted through git itself rather than by reading `info/exclude`,
    /// because what matters is git's answer, not the rule's spelling. Both
    /// modes, because in place the directory is in the user's own folder --
    /// which is precisely where an untracked file is least welcome.
    #[tokio::test]
    async fn a_plans_pictures_are_invisible_to_git_in_every_mode() {
        let dir = repo().await;
        let root = dir.path();

        for mode in [WorkspaceMode::Fresh, WorkspaceMode::InPlace] {
            let workspace = prepare(root, &mode, "look-at-the-sidebar").await.unwrap();
            let workspace_root = PathBuf::from(&workspace.path);

            let shots = workspace_root.join(crate::tools::ARTIFACT_DIR);
            std::fs::create_dir_all(&shots).unwrap();
            std::fs::write(shots.join("browser-screenshot-1.png"), "pretend").unwrap();

            assert_eq!(
                git(&workspace_root, &["status", "--porcelain"])
                    .await
                    .unwrap()
                    .trim(),
                "",
                "{mode:?}: a picture git can see is a picture the court deletes"
            );

            // And the consequence that would outlive the plan: the commit both
            // endings make must not carry it.
            assert!(
                !commit_pending(&workspace_root, "Kingdom: work in progress")
                    .await
                    .unwrap(),
                "{mode:?}: there must be nothing to commit, so nothing to land"
            );
        }
    }

    /// The user asked to be fenced in. Quietly handing him the live folder
    /// instead would be the exact failure this feature exists to prevent, so a
    /// non-git city must refuse rather than downgrade.
    #[tokio::test]
    async fn a_city_without_git_refuses_isolation_rather_than_downgrading() {
        let dir = tempfile::tempdir().unwrap();
        let outcome = prepare(dir.path(), &WorkspaceMode::Fresh, "tidy-the-sidebar").await;
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
    /// must leave the user's project exactly as it was found.
    ///
    /// A half-merged working tree discovered hours later is far worse than a
    /// refusal read now -- the user would have no way of telling which of the
    /// files in front of him were his own work and which an agent's. So the
    /// abort matters as much as the refusal, and both are pinned here along
    /// with the worktree surviving, so the work is still there to retry with.
    #[tokio::test]
    async fn a_conflicted_merge_leaves_the_city_exactly_as_it_was() {
        let dir = repo().await;
        let root = dir.path();

        let workspace = prepare(root, &WorkspaceMode::Fresh, "tidy-the-sidebar")
            .await
            .unwrap();
        let worktree = PathBuf::from(&workspace.path);

        // The same file, edited two different ways.
        commit(&worktree, "README.md", "the plan's version").await;
        commit(root, "README.md", "the King's version").await;

        let before = head_of(root).await.unwrap();

        let finish = merge(root, &workspace).await.unwrap();
        // `Diverged` and not `Refused`: git was asked and declined, which is the
        // one merge refusal an agent can clear. The chamber offers to send the
        // court after the base branch on exactly this distinction, so a
        // regression that collapsed the two would take that button away.
        let Finish::Diverged(why) = finish else {
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

    /// The other side of that distinction, and the reason it is worth a variant.
    ///
    /// A city with the wrong branch checked out is refused **before git is ever
    /// asked to merge**, and it is the user's to clear: no agent can switch the
    /// branch in his own working copy, and moving it out from under him is
    /// precisely the collision this product exists to prevent. So it stays
    /// `Refused`, and the chamber offers no "catch up" button beside it -- which
    /// would point at the wrong hand entirely.
    #[tokio::test]
    async fn the_wrong_branch_checked_out_is_the_kings_to_clear_not_the_courts() {
        let dir = repo().await;
        let root = dir.path();

        let workspace = prepare(root, &WorkspaceMode::Fresh, "tidy-the-sidebar")
            .await
            .unwrap();

        // The King wandered off `main` in his own checkout, which is his to do.
        git(root, &["checkout", "-b", "experiment"]).await.unwrap();

        let finish = merge(root, &workspace).await.unwrap();
        let Finish::Refused(why) = finish else {
            panic!(
                "a city on the wrong branch is refused, and is not a divergence \
                 an agent could merge away"
            );
        };
        assert!(
            why.contains("main") && why.contains("experiment"),
            "the refusal must name both branches, or he cannot act on it: {why}"
        );

        // And a plan that never recorded a base is the third case: nowhere
        // certain to merge it, so nobody -- King or court -- can be pointed at a
        // next move, and it stays `Refused` too.
        let mut baseless = workspace.clone();
        baseless.base = None;
        let Finish::Refused(_) = merge(root, &baseless).await.unwrap() else {
            panic!("a plan with no recorded base has no divergence to settle");
        };
    }

    /// The happy path, and the answer to the question this module deferred:
    /// a worktree is disposable exactly when its work has landed.
    #[tokio::test]
    async fn a_clean_merge_lands_the_work_and_disposes_of_the_worktree() {
        let dir = repo().await;
        let root = dir.path();

        let workspace = prepare(root, &WorkspaceMode::Fresh, "tidy-the-sidebar")
            .await
            .unwrap();
        let worktree = PathBuf::from(&workspace.path);
        let branch = workspace.branch.clone().unwrap();

        // Uncommitted on purpose: the user should not have to tidy up before he
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

        let workspace = prepare(root, &WorkspaceMode::Fresh, "tidy-the-sidebar")
            .await
            .unwrap();
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

    /// A worktree is hidden from the repository; the service manifest is not.
    ///
    /// Both halves matter and they pull against each other, which is why this
    /// is pinned against real `git status` rather than by reading the exclude
    /// file back. The obvious rule -- `.kingdom/` -- silently fails the second
    /// half: git excludes the *directory* and never looks inside, so a
    /// negation for the manifest has nothing to act on and the project cannot
    /// commit the file that says what it needs to run.
    #[tokio::test]
    async fn the_service_manifest_stays_visible_while_worktrees_are_hidden() {
        let dir = repo().await;
        let root = dir.path();

        prepare(root, &WorkspaceMode::Fresh, "raise-the-well")
            .await
            .unwrap();

        let manifest = root.join(kingdom_core::services::MANIFEST_PATH);
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        std::fs::write(&manifest, "[[service]]\nname = \"db\"\n").unwrap();

        let status = git(root, &["status", "--porcelain", "--untracked-files=all"])
            .await
            .expect("git status");

        assert!(
            status.contains("services.toml"),
            "the project must be able to commit its own service manifest; \
             git said: {status:?}"
        );
        assert!(
            !status.contains("worktree") && !status.contains("draft"),
            "Kingdom's scratch folder must stay out of the repository; \
             git said: {status:?}"
        );
    }

    /// A repository excluded by the old rule is brought forward.
    ///
    /// Every project any previous Kingdom touched carries a bare `.kingdom/`
    /// line. Left in place it still hides the directory and wins over the new
    /// rule, so those projects -- the existing ones, the ones that matter --
    /// would never be able to commit a manifest.
    #[tokio::test]
    async fn a_repository_excluded_by_the_old_rule_is_brought_forward() {
        let dir = repo().await;
        let root = dir.path();

        // Exactly what a previous version of Kingdom wrote.
        let exclude = root.join(".git").join("info").join("exclude");
        std::fs::create_dir_all(exclude.parent().unwrap()).unwrap();
        std::fs::write(&exclude, "# something the user added\n*.tmp\n.kingdom/\n").unwrap();

        prepare(root, &WorkspaceMode::Fresh, "raise-the-well")
            .await
            .unwrap();

        let rules = std::fs::read_to_string(&exclude).unwrap();
        assert!(
            !rules.lines().any(|l| l.trim() == ".kingdom/"),
            "the old rule must go, or it still hides the manifest: {rules:?}"
        );
        assert!(
            rules.lines().any(|l| l.trim() == "*.tmp"),
            "the user's own rules must survive: {rules:?}"
        );

        let manifest = root.join(kingdom_core::services::MANIFEST_PATH);
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        std::fs::write(&manifest, "[[service]]\nname = \"db\"\n").unwrap();
        let status = git(root, &["status", "--porcelain", "--untracked-files=all"])
            .await
            .expect("git status");
        assert!(status.contains("services.toml"), "git said: {status:?}");
    }

    /// The draft dies with the checkout, which is why it must be read first.
    ///
    /// The whole timing constraint on filing a plan, pinned against real git
    /// rather than argued for in a comment. `.kingdom/` is excluded from the
    /// repository, so the draft is never committed and cannot be recovered from
    /// the branch or the patch -- and `worktree remove --force` then deletes
    /// it. Read before the disposal it survives; read after, it is gone.
    ///
    /// If this ever fails, `api::finish_plan` is filing nothing and every plan
    /// archived without approval loses its document.
    #[tokio::test]
    async fn a_draft_is_destroyed_with_the_worktree_that_held_it() {
        let dir = repo().await;
        let root = dir.path();

        let workspace = prepare(root, &WorkspaceMode::Fresh, "tidy-the-sidebar")
            .await
            .unwrap();
        let worktree = PathBuf::from(&workspace.path);

        let draft = worktree.join(crate::tools::propose_plan::DRAFT);
        std::fs::create_dir_all(draft.parent().unwrap()).unwrap();
        std::fs::write(&draft, "# The plan\n\nWhat I would do.\n").unwrap();

        // What `finish_plan` does, in the order it does it.
        let kept = crate::tools::propose_plan::draft_body(&workspace)
            .expect("the draft is readable while the worktree stands");

        let patch_path = dir.path().join("archive").join("plan-1.patch");
        archive(root, &workspace, &patch_path).await.unwrap();

        assert!(
            !draft.exists(),
            "the draft goes with the checkout -- this is what filing exists to survive"
        );
        assert!(
            crate::tools::propose_plan::draft_body(&workspace).is_none(),
            "and there is nothing left to read afterwards"
        );
        assert!(
            kept.contains("What I would do."),
            "the copy read beforehand is the one that survives: {kept}"
        );

        // Not merely deleted -- never recoverable from git either, which is why
        // reading it out is the only option.
        let patch = std::fs::read_to_string(&patch_path).unwrap_or_default();
        assert!(
            !patch.contains("draft.md"),
            "an excluded file is never committed, so the patch cannot carry it"
        );
    }

    /// The destructive edge of pruning: a plan working on a branch the user
    /// named is working on *his* branch, which almost certainly has a life
    /// beyond this plan. Tidying up after ourselves must never reach that far.
    #[tokio::test]
    async fn archiving_never_deletes_a_branch_the_user_named() {
        let dir = repo().await;
        let root = dir.path();

        git(root, &["branch", "fix/parser"]).await.unwrap();
        let workspace = prepare(
            root,
            &WorkspaceMode::Branch("fix/parser".into()),
            "fix-the-parser",
        )
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

    /// A repository with a remote that carries `refs/pull/7/head`, the way
    /// GitHub does, one commit ahead of `main`. Returns the city and the sha.
    async fn repo_with_a_pull_request() -> (tempfile::TempDir, String) {
        let dir = repo().await;
        let root = dir.path();
        let remote = root.join(".remote.git");
        let remote_arg = remote.to_string_lossy().to_string();
        git(root, &["init", "-q", "--bare", &remote_arg])
            .await
            .unwrap();
        git(root, &["remote", "add", "origin", &remote_arg])
            .await
            .unwrap();
        git(root, &["push", "-q", "origin", "main"]).await.unwrap();

        // The author's change, made on a branch and pushed only as the pull
        // ref -- the King has no local branch for it, exactly as with a fork.
        git(root, &["checkout", "-q", "-b", "author"])
            .await
            .unwrap();
        std::fs::write(root.join("login.rs"), "fn login() {}").unwrap();
        git(root, &["add", "login.rs"]).await.unwrap();
        git(root, &["commit", "-q", "-m", "Add login"])
            .await
            .unwrap();
        let sha = head_of(root).await.unwrap();
        git(root, &["push", "-q", "origin", "author:refs/pull/7/head"])
            .await
            .unwrap();
        git(root, &["checkout", "-q", "main"]).await.unwrap();
        git(root, &["branch", "-D", "author"]).await.unwrap();
        (dir, sha)
    }

    fn pull_seven(sha: &str) -> kingdom_core::PullRequestRef {
        kingdom_core::PullRequestRef {
            number: 7,
            title: "Add login".into(),
            url: "https://github.com/o/r/pull/7".into(),
            base: "main".into(),
            head: "author".into(),
            head_sha: sha.into(),
            remote: "origin".into(),
        }
    }

    /// The whole checkout half of a review: the pull request lands in a
    /// worktree of its own at exactly its commit, on a scratch branch, with no
    /// stray fetch ref left behind -- and the King's checkout does not move.
    #[tokio::test]
    async fn a_pull_request_is_checked_out_at_its_own_commit() {
        let (dir, sha) = repo_with_a_pull_request().await;
        let root = dir.path();

        let mode = WorkspaceMode::PullRequest(pull_seven("stale"));
        let workspace = prepare(root, &mode, "").await.unwrap();
        let worktree = PathBuf::from(&workspace.path);

        assert_eq!(head_of(&worktree).await.unwrap(), sha);
        assert!(
            worktree.join("login.rs").is_file(),
            "the change is there to run"
        );
        assert_eq!(workspace.branch.as_deref(), Some("kingdom/pr-7-review"));
        assert_eq!(workspace.base.as_deref(), Some("main"));
        assert_eq!(
            workspace.mode.pull_request().map(|p| p.head_sha.as_str()),
            Some(sha.as_str()),
            "the record names the commit actually checked out, not the stale one asked for"
        );
        let scratch = git(root, &["for-each-ref", "refs/kingdom"]).await.unwrap();
        assert!(scratch.trim().is_empty(), "left behind: {scratch}");
        assert!(!root.join("login.rs").exists(), "the King's checkout moved");

        // A second review of the same pull request does not collide.
        let again = prepare(root, &mode, "").await.unwrap();
        assert_eq!(again.branch.as_deref(), Some("kingdom/pr-7-review-2"));
        assert_ne!(again.path, workspace.path);
    }

    /// Kingdom never merges a pull request, even when asked directly.
    #[tokio::test]
    async fn a_review_is_never_merged() {
        let (dir, sha) = repo_with_a_pull_request().await;
        let root = dir.path();
        let workspace = prepare(root, &WorkspaceMode::PullRequest(pull_seven(&sha)), "")
            .await
            .unwrap();
        let before = head_of(root).await.unwrap();

        let finish = merge(root, &workspace).await.unwrap();
        assert!(matches!(finish, Finish::Refused(ref why) if why.contains("does not merge")));
        assert_eq!(head_of(root).await.unwrap(), before, "main moved");
        assert!(
            PathBuf::from(&workspace.path).is_dir(),
            "the review was torn down"
        );
    }

    /// Closing a review clears the checkout and its scratch branch -- and keeps
    /// anything the court left in it, rather than throwing it away.
    #[tokio::test]
    async fn closing_a_review_clears_it_and_keeps_stray_edits() {
        let (dir, sha) = repo_with_a_pull_request().await;
        let root = dir.path();
        let workspace = prepare(root, &WorkspaceMode::PullRequest(pull_seven(&sha)), "")
            .await
            .unwrap();
        let worktree = PathBuf::from(&workspace.path);
        std::fs::write(worktree.join("login.rs"), "fn login() { todo!() }").unwrap();

        let patch_path = dir.path().join("archive").join("plan-9.patch");
        let finish = close_review(root, &workspace, &patch_path).await.unwrap();
        let Finish::Settled(Outcome::Reviewed {
            number,
            head,
            patch,
        }) = finish
        else {
            panic!("closing a review must settle as reviewed");
        };

        assert_eq!((number, head.as_str()), (7, sha.as_str()));
        assert_eq!(patch.as_deref(), Some(&*patch_path.to_string_lossy()));
        assert!(std::fs::read_to_string(&patch_path)
            .unwrap()
            .contains("todo!()"));
        assert!(!worktree.exists(), "the checkout is gone");
        assert!(
            !git(root, &["branch", "--list", "kingdom/pr-7-review"])
                .await
                .unwrap()
                .contains("pr-7"),
            "the scratch branch is gone"
        );
    }

    /// The changes drawer shows a review exactly the pull request's own
    /// change -- measured against its base on the remote, so a commit the King
    /// has on his local `main` and the remote does not is not attributed to
    /// the author.
    #[tokio::test]
    async fn a_reviews_changes_are_the_pull_requests_alone() {
        let (dir, sha) = repo_with_a_pull_request().await;
        let root = dir.path();
        // Local work the remote has never seen.
        std::fs::write(root.join("mine.rs"), "fn mine() {}").unwrap();
        git(root, &["add", "mine.rs"]).await.unwrap();
        git(root, &["commit", "-q", "-m", "Mine"]).await.unwrap();

        let workspace = prepare(root, &WorkspaceMode::PullRequest(pull_seven(&sha)), "")
            .await
            .unwrap();
        let summary = crate::review::changes(&workspace).await;
        let paths: Vec<&str> = summary.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["login.rs"], "{summary:?}");
    }

    /// A clean review leaves no patch behind.
    #[tokio::test]
    async fn a_clean_review_closes_without_a_patch() {
        let (dir, sha) = repo_with_a_pull_request().await;
        let root = dir.path();
        let workspace = prepare(root, &WorkspaceMode::PullRequest(pull_seven(&sha)), "")
            .await
            .unwrap();
        let patch_path = dir.path().join("archive").join("plan-10.patch");

        let finish = close_review(root, &workspace, &patch_path).await.unwrap();
        assert!(matches!(
            finish,
            Finish::Settled(Outcome::Reviewed { patch: None, .. })
        ));
        assert!(!patch_path.exists());
    }
}
