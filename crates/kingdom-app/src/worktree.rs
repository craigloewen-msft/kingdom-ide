//! Preparing a place on disk for a plan to work.
//!
//! Server-only, and the second place in the codebase that touches the disk (the
//! other being [`crate::scan`]). Everything here shells out to `git` rather than
//! linking a git library: the operations are few, the CLI's behaviour is the
//! contract users already know, and its refusals are written in words worth
//! showing the King verbatim.
//!
//! Nothing here removes a worktree. They persist under `.kingdom/` so the King
//! can inspect or merge them; deciding when one is disposable is a separate
//! question, and guessing at it would throw away real work.

use kingdom_core::{Workspace, WorkspaceMode};
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
    })
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
    /// land in the same directory. Everything downstream -- the leases, the
    /// map's contention, the King's trust that an agent is fenced in -- rests on
    /// this being true on disk rather than merely in the type.
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
}
