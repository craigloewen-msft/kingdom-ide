//! Reading a project's pull requests from GitHub, through `gh`.
//!
//! Server-only. Shells out to the GitHub CLI for the reason `worktree.rs`
//! shells out to git: the King has already logged it in, its refusals are worth
//! showing verbatim, and it means Kingdom holds no GitHub token of its own.
//!
//! **Read-only, and that is the whole of its surface.** Two calls exist --
//! [`list`] and [`view`] -- and both only ask. Nothing in Kingdom posts a
//! review, a comment or a status to GitHub: the King asked for findings he can
//! read and act on himself, and a tool that could post would be one bug away
//! from speaking for him in public. A review plan's own commands are cut off
//! from GitHub as well; see `tools::child_environment`.
//!
//! Every ordinary absence -- no `gh`, not logged in, no GitHub remote -- is an
//! [`PullRequestList::Unavailable`] with what to do about it, never an `Err`.

use kingdom_core::{PullRequest, PullRequestList, ReviewDecision};
use serde_json::Value;
use std::path::Path;
use std::time::Duration;

/// The variable that overrides which `gh` is run.
///
/// The sibling of `KINGDOM_CHROME_EXECUTABLE`: for a machine where `gh` is not
/// on `PATH`, and for rehearsing this feature offline against a script that
/// prints the JSON `gh` would.
pub const GH_VAR: &str = "KINGDOM_GH";

/// How long one `gh` call may take before the King is told it did not answer.
///
/// Generous, because the first call after a while can be slow on a cold token
/// refresh, and short enough that a hung network does not leave the screen
/// spinning forever.
const PATIENCE: Duration = Duration::from_secs(30);

/// How many open pull requests one listing asks for. A screen of a hundred is
/// already more than anyone reviews in a sitting.
const MOST_LISTED: usize = 100;

/// The fields the list asks for. Named once so the parser and the request
/// cannot disagree about what came back.
const LIST_FIELDS: &str = "number,title,author,headRefName,baseRefName,headRefOid,url,isDraft,\
                           additions,deletions,changedFiles,updatedAt,reviewDecision,\
                           isCrossRepository";

/// Every open pull request on the repository this project's remotes point at.
pub async fn list(city_root: &Path) -> PullRequestList {
    let repository = match repository(city_root).await {
        Ok(r) => r,
        Err(why) => return PullRequestList::Unavailable(why),
    };

    let limit = MOST_LISTED.to_string();
    let out = gh(
        city_root,
        &[
            "pr",
            "list",
            "--repo",
            &repository,
            "--state",
            "open",
            "--limit",
            &limit,
            "--json",
            LIST_FIELDS,
        ],
    )
    .await;

    match out.and_then(|json| parse_list(&json)) {
        Ok(pulls) => PullRequestList::Listed { repository, pulls },
        Err(why) => PullRequestList::Unavailable(why),
    }
}

/// One pull request, read fresh -- description included -- together with the
/// git remote it can be fetched from.
///
/// Read again at the moment a review starts rather than taken from the list
/// the King was looking at, because the list may be minutes old: a push in the
/// meantime would otherwise brief the court on one commit and check out
/// another.
pub async fn view(city_root: &Path, number: u64) -> Result<(PullRequest, String), String> {
    let repository = repository(city_root).await?;
    let fields = format!("{LIST_FIELDS},body");
    let json = gh(
        city_root,
        &[
            "pr",
            "view",
            &number.to_string(),
            "--repo",
            &repository,
            "--json",
            &fields,
        ],
    )
    .await?;
    let value: Value = serde_json::from_str(&json)
        .map_err(|e| format!("gh answered with something unreadable: {e}"))?;
    let pull = parse_pull(&value).ok_or_else(|| {
        "gh answered, but not with a pull request Kingdom could read.".to_string()
    })?;

    let remotes = git_remotes(city_root).await;
    let remote = remote_for(&remotes, &repository).unwrap_or_else(|| "origin".to_string());
    Ok((pull, remote))
}

/// `owner/name` of the repository `gh` resolves this project to.
async fn repository(city_root: &Path) -> Result<String, String> {
    if !city_root.join(".git").exists() {
        return Err("This project is not a git repository, so it has no pull requests.".into());
    }
    let json = gh(city_root, &["repo", "view", "--json", "nameWithOwner"]).await?;
    serde_json::from_str::<Value>(&json)
        .ok()
        .and_then(|v| v.get("nameWithOwner")?.as_str().map(str::to_string))
        .filter(|name| !name.is_empty())
        .ok_or_else(|| "gh could not say which GitHub repository this project is.".to_string())
}

/// Runs `gh` in the project, returning its stdout or a sentence for the King.
async fn gh(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let program = std::env::var(GH_VAR)
        .ok()
        .filter(|p| !p.trim().is_empty())
        .unwrap_or_else(|| "gh".to_string());

    let mut command = tokio::process::Command::new(&program);
    command
        .args(args)
        .current_dir(cwd)
        // Never a prompt: there is no terminal for it to wait on, and a `gh`
        // waiting on one would hold this request until the timeout.
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);

    let output = match tokio::time::timeout(PATIENCE, command.output()).await {
        Err(_) => {
            return Err(format!(
                "GitHub did not answer within {} seconds. Check the network and try again.",
                PATIENCE.as_secs()
            ))
        }
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(
                "The GitHub CLI (`gh`) is not installed, and Kingdom reads pull \
                        requests through it. Install it from https://cli.github.com and run \
                        `gh auth login`."
                    .to_string(),
            )
        }
        Ok(Err(e)) => return Err(format!("`{program}` could not be run: {e}")),
        Ok(Ok(output)) => output,
    };

    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).to_string());
    }
    Err(trouble(&String::from_utf8_lossy(&output.stderr)))
}

/// `gh`'s refusal, in words that say what to do about it.
///
/// Matched on the text because `gh` has no distinct exit codes for these. A
/// miss costs little: the King is shown `gh`'s own words instead.
fn trouble(stderr: &str) -> String {
    let said = stderr.trim();
    let lower = said.to_lowercase();
    if lower.contains("gh auth login") || lower.contains("not logged in") {
        return "The GitHub CLI is not logged in. Run `gh auth login` in a terminal, then \
                refresh."
            .to_string();
    }
    if lower.contains("no git remotes")
        || lower.contains("none of the git remotes")
        || lower.contains("not a git repository")
    {
        return "This project has no GitHub remote, so it has no pull requests to review."
            .to_string();
    }
    if said.is_empty() {
        "gh failed without saying why.".to_string()
    } else {
        format!("gh refused: {said}")
    }
}

/// A listing, read.
pub(crate) fn parse_list(json: &str) -> Result<Vec<PullRequest>, String> {
    let value: Value = serde_json::from_str(json)
        .map_err(|e| format!("gh answered with something unreadable: {e}"))?;
    let rows = value
        .as_array()
        .ok_or_else(|| "gh answered with something other than a list.".to_string())?;
    Ok(rows.iter().filter_map(parse_pull).collect())
}

/// One pull request out of `gh`'s JSON. `None` only when it has no number,
/// which is the one field nothing can stand in for.
fn parse_pull(v: &Value) -> Option<PullRequest> {
    let text = |key: &str| {
        v.get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let count = |key: &str| v.get(key).and_then(Value::as_u64).unwrap_or(0);
    let flag = |key: &str| v.get(key).and_then(Value::as_bool).unwrap_or(false);

    Some(PullRequest {
        number: v.get("number")?.as_u64()?,
        title: text("title"),
        author: v
            .get("author")
            .and_then(|a| a.get("login"))
            .and_then(Value::as_str)
            .unwrap_or("someone")
            .to_string(),
        head: text("headRefName"),
        base: text("baseRefName"),
        head_sha: text("headRefOid"),
        url: text("url"),
        draft: flag("isDraft"),
        additions: count("additions"),
        deletions: count("deletions"),
        changed_files: count("changedFiles"),
        updated_at: text("updatedAt"),
        decision: ReviewDecision::from_github(&text("reviewDecision")),
        from_fork: flag("isCrossRepository"),
        body: text("body"),
    })
}

/// `git remote -v`, or nothing.
async fn git_remotes(cwd: &Path) -> String {
    tokio::process::Command::new("git")
        .args(["remote", "-v"])
        .current_dir(cwd)
        .output()
        .await
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default()
}

/// Which git remote points at `repository` (`owner/name`), read from
/// `git remote -v`.
///
/// Needed because a fork-and-upstream checkout has two remotes and the pull
/// request lives on only one of them. Compared case-insensitively, because
/// GitHub is.
pub(crate) fn remote_for(remotes: &str, repository: &str) -> Option<String> {
    let wanted = repository.trim().to_lowercase();
    remotes.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let name = parts.next()?;
        let url = parts.next()?;
        (slug_of(url)? == wanted).then(|| name.to_string())
    })
}

/// `owner/name` out of any of the shapes a GitHub remote URL comes in:
/// `https://github.com/o/r.git`, `git@github.com:o/r.git`,
/// `ssh://git@github.com/o/r`.
fn slug_of(url: &str) -> Option<String> {
    let url = url.trim().trim_end_matches('/');
    let url = url.strip_suffix(".git").unwrap_or(url);
    // The path is whatever follows the host, which is after the last `:` of an
    // scp-style address or the third `/` of a URL.
    let path = match url.split_once("://") {
        Some((_, rest)) => rest.split_once('/')?.1,
        None => url.rsplit_once(':')?.1,
    };
    let mut segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if segments.len() < 2 {
        return None;
    }
    let name = segments.pop()?;
    let owner = segments.pop()?;
    Some(format!("{owner}/{name}").to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    const LISTING: &str = r#"[
      {"number": 12, "title": "Add the login page", "author": {"login": "ada"},
       "headRefName": "login", "baseRefName": "main", "headRefOid": "abc123",
       "url": "https://github.com/o/r/pull/12", "isDraft": true, "additions": 40,
       "deletions": 3, "changedFiles": 4, "updatedAt": "2025-05-01T10:00:00Z",
       "reviewDecision": "REVIEW_REQUIRED", "isCrossRepository": true},
      {"title": "no number, so not a pull request"},
      {"number": 9, "title": "Older", "author": null, "reviewDecision": ""}
    ]"#;

    #[test]
    fn a_listing_is_read_field_by_field() {
        let pulls = parse_list(LISTING).unwrap();
        assert_eq!(pulls.len(), 2, "the row with no number is dropped");
        let first = &pulls[0];
        assert_eq!(first.number, 12);
        assert_eq!(first.author, "ada");
        assert_eq!(
            (first.head.as_str(), first.base.as_str()),
            ("login", "main")
        );
        assert_eq!(first.head_sha, "abc123");
        assert!(first.draft && first.from_fork);
        assert_eq!(
            (first.additions, first.deletions, first.changed_files),
            (40, 3, 4)
        );
        assert_eq!(first.decision, Some(ReviewDecision::ReviewRequired));

        let second = &pulls[1];
        assert_eq!(second.author, "someone", "a deleted account still reads");
        assert_eq!(second.decision, None);
    }

    #[test]
    fn something_that_is_not_a_list_is_said_rather_than_swallowed() {
        assert!(parse_list("{}").is_err());
        assert!(parse_list("not json").is_err());
    }

    /// The fork-and-upstream checkout: the pull request lives on `upstream`,
    /// and fetching from `origin` would find nothing.
    #[test]
    fn the_remote_is_the_one_pointing_at_the_repository() {
        let remotes = "origin\tgit@github.com:me/Kingdom.git (fetch)\n\
                       origin\tgit@github.com:me/Kingdom.git (push)\n\
                       upstream\thttps://github.com/Crown/kingdom (fetch)\n\
                       upstream\thttps://github.com/Crown/kingdom (push)\n";
        assert_eq!(
            remote_for(remotes, "crown/Kingdom").as_deref(),
            Some("upstream")
        );
        assert_eq!(remote_for(remotes, "me/kingdom").as_deref(), Some("origin"));
        assert_eq!(remote_for(remotes, "someone/else"), None);
    }

    #[test]
    fn every_shape_of_github_url_names_its_repository() {
        for url in [
            "https://github.com/o/r.git",
            "https://github.com/o/r/",
            "git@github.com:o/r.git",
            "ssh://git@github.com/o/r",
            "ssh://git@github.com:22/o/r.git",
        ] {
            assert_eq!(slug_of(url).as_deref(), Some("o/r"), "{url}");
        }
    }

    #[test]
    fn gh_refusals_say_what_to_do() {
        assert!(
            trouble("To get started with GitHub CLI, please run:  gh auth login")
                .contains("not logged in")
        );
        assert!(trouble(
            "none of the git remotes configured for this repository point to a known GitHub host"
        )
        .contains("no GitHub remote"));
        assert_eq!(trouble(""), "gh failed without saying why.");
        assert!(trouble("HTTP 502").contains("HTTP 502"));
    }
}
