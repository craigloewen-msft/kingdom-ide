//! Tools: what the court can do with its own hands.
//!
//! Server-only, for the same reason `llm/` is. Named for what it is rather than
//! given a metaphor noun -- the metaphor is carried by [`kingdom_core::Deed`],
//! and inventing a second name for the plumbing would only obscure where the
//! subprocess is spawned. The precedent is set at the top of `llm/mod.rs`.
//!
//! # The boundary
//!
//! Every tool is rooted at the plan's workspace, and [`Workshop`] is the only
//! thing that knows where that is. A tool receives paths already resolved and
//! checked; it cannot reach outside because it is never told how to.
//!
//! That check lives here, at the seam, rather than in each tool. A check every
//! tool has to remember is a check the next tool will forget, and the tool that
//! forgets is the one that quietly edits a file in somebody else's checkout.
//!
//! Read [`Workshop::resolve`] before adding a tool that touches the filesystem,
//! and [`Workshop::root`] before adding one that spawns a process -- the
//! guarantee is real for the first and deliberately weaker for the second.

pub mod ask_user_question;
pub mod bash;
pub mod browser;
pub mod patch;
pub mod read_file;
pub mod read_image;
pub mod search;
pub mod think;
pub mod tmux;

use kingdom_core::{DeedOutcome, Workspace};
use serde_json::Value;
use std::path::{Component, Path, PathBuf};

/// Every tool the court has.
///
/// A plain list rather than a registry with registration macros, for the same
/// reason `llm::providers()` is: a literal is the version of this a reader can
/// take in at a glance, and a tool that is not in this list does not exist.
/// Order is the order the model is shown them in.
pub fn all() -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(think::Think),
        Box::new(read_file::ReadFile),
        Box::new(search::Search),
        Box::new(bash::Bash),
        Box::new(tmux::TmuxRun),
        Box::new(tmux::Tmux),
        Box::new(patch::Patch),
        Box::new(browser::BrowserNavigate),
        Box::new(browser::BrowserClick),
        Box::new(browser::BrowserType),
        Box::new(browser::BrowserKeyPress),
        Box::new(browser::BrowserEval),
        Box::new(browser::BrowserWaitForSelector),
        Box::new(browser::BrowserTakeScreenshot),
        Box::new(read_image::ReadImage),
        Box::new(browser::BrowserResize),
        Box::new(browser::BrowserRecentConsoleLogs),
        Box::new(browser::BrowserClearConsoleLogs),
        Box::new(ask_user_question::AskUserQuestion),
    ]
}

/// Runs one tool call by name, inside the workspace's bounds.
///
/// The single point where a name from a model becomes work on a real machine.
/// An unknown name is a refusal reported back to the model rather than an
/// error: models hallucinate tool names, and being told "there is no such tool"
/// is something a model can recover from in one turn.
pub async fn invoke(tool: &str, input: Value, shop: &Workshop) -> DeedOutcome {
    match all().into_iter().find(|t| t.name() == tool) {
        Some(t) => t.run(input, shop).await,
        None => Refusal::NoSuchTool(tool.to_string()).into(),
    }
}

/// What a tool may do, and where.
///
/// Handed to every call. Holds the workspace rather than a bare path so a tool
/// can tell an isolated plan from one working directly in the city -- the
/// guarantee it is operating under differs between them, and a tool that cannot
/// tell cannot say so.
///
/// It also carries *which call this is*. Most tools never look: a command does
/// not care which plan asked for it. But a tool that has to reach back out --
/// to put a question in front of the King and wait for his answer -- needs
/// something the browser can name when it answers, and the deed it is already
/// being rendered as is exactly that. Minting a second identifier would mean
/// keeping two in step for no gain.
#[derive(Debug, Clone)]
pub struct Workshop {
    workspace: Workspace,
    plan: kingdom_core::PlanId,
    /// The deed this call is recorded as, once it is running. `None` outside a
    /// turn, which is the case tests construct.
    deed: Option<String>,
}

/// A refusal: the tool would not run, and why.
///
/// The reason is written for the *model*, not for the King. It is fed back as
/// the call's result, so it has to be actionable -- "that path is outside the
/// workspace" lets a model correct itself, where a bare "refused" earns a retry
/// of exactly the same call.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    #[error("{path} is outside this plan's workspace. Everything this plan may \
             touch is under {root}; use a path inside it.")]
    OutsideWorkspace { path: String, root: String },

    #[error("{tool} was called with arguments it cannot read: {detail}")]
    BadArguments { tool: String, detail: String },

    #[error("There is no tool called {0} here.")]
    NoSuchTool(String),

    #[error("{0}")]
    Refused(String),
}

impl From<Refusal> for DeedOutcome {
    fn from(refusal: Refusal) -> Self {
        DeedOutcome::Refused {
            reason: refusal.to_string(),
        }
    }
}

impl Workshop {
    pub fn new(workspace: Workspace) -> Self {
        Self {
            workspace,
            plan: kingdom_core::PlanId::new(String::new()),
            deed: None,
        }
    }

    /// Names the plan these tools work for.
    pub fn for_plan(mut self, plan: kingdom_core::PlanId) -> Self {
        self.plan = plan;
        self
    }

    /// A copy of this workshop bound to one recorded call.
    ///
    /// Cloned per call rather than mutated, so a tool cannot see the id of a
    /// call that is not its own -- which is what would let one tool answer
    /// another's question.
    pub fn for_deed(&self, deed: impl Into<String>) -> Self {
        Self {
            deed: Some(deed.into()),
            ..self.clone()
        }
    }

    /// Which plan this call belongs to.
    pub fn plan(&self) -> &kingdom_core::PlanId {
        &self.plan
    }

    /// The deed this call is recorded as.
    pub fn deed(&self) -> Option<&str> {
        self.deed.as_deref()
    }

    /// The directory everything this plan does happens under.
    ///
    /// **This is a path-level guarantee, not a sandbox.** It is enforced for
    /// every path a tool is given, which contains the tools that take paths
    /// entirely. It does *not* contain a shell: `bash` is handed this as its
    /// working directory, and a command that types an absolute path or `cd /`
    /// goes wherever it likes. That is a real hole and it is stated here rather
    /// than implied away, because a guarantee people believe in and that does
    /// not hold is worse than a limit they can see. Closing it means an
    /// OS-level sandbox, which is a deliberate later decision.
    pub fn root(&self) -> &Path {
        Path::new(&self.workspace.path)
    }

    /// True when this plan works in a worktree of its own.
    ///
    /// False means the King chose to work directly in the city, so the boundary
    /// below is the project itself and gives no isolation from his own
    /// checkout. That is a choice he made knowingly and the tools honour it --
    /// but it must be a *visible* weaker guarantee rather than an accident,
    /// which is why this is exposed rather than hidden.
    pub fn is_isolated(&self) -> bool {
        self.workspace.is_isolated()
    }

    /// Turns a path from a model into one a tool may actually use.
    ///
    /// Relative paths are taken as relative to the workspace; absolute paths
    /// are permitted only if they are already inside it.
    ///
    /// The check is done on a *lexically normalised* path, and `..` is resolved
    /// here rather than handed to the filesystem, so `a/../../etc/passwd` is
    /// rejected on its way in. Canonicalising instead would be stricter about
    /// symlinks but only works on paths that already exist -- which would make
    /// "create a new file" unexpressible, so the normalisation is lexical and
    /// the symlink caveat is real: a symlink already inside the workspace and
    /// pointing out of it is followed. Worktrees are cut by us and contain no
    /// such link unless something put one there.
    pub fn resolve(&self, path: &str) -> Result<PathBuf, Refusal> {
        let root = normalise(self.root());
        let candidate = Path::new(path);

        let joined = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            root.join(candidate)
        };

        let resolved = normalise(&joined);

        if !resolved.starts_with(&root) {
            return Err(Refusal::OutsideWorkspace {
                path: path.to_string(),
                root: root.display().to_string(),
            });
        }

        Ok(resolved)
    }
}

/// Collapses `.` and `..` without touching the filesystem.
///
/// Deliberately not [`std::fs::canonicalize`]: that requires the path to exist,
/// and a tool must be able to name a file it is about to create.
fn normalise(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            // Popping past the root leaves it empty, which then fails the
            // `starts_with` check above -- an escape attempt cannot normalise
            // its way into looking like the root itself.
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// One thing the court can do.
///
/// Stateless: every tool is a singleton and all per-call context arrives in the
/// [`Workshop`]. That is what lets one instance serve every plan at once
/// without a tool ever holding a plan's path in a field, which is the mistake
/// that would make the boundary above a suggestion.
#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    /// The name the model calls it by, and the name recorded on the deed.
    fn name(&self) -> &'static str;

    /// What it does, written for a model.
    fn description(&self) -> String;

    /// JSON Schema for the arguments.
    fn input_schema(&self) -> Value;

    /// Runs the tool.
    ///
    /// Returns a [`DeedOutcome`] rather than a `Result` because both endings are
    /// results the model must be *told*: a refusal it never hears about is a
    /// call it makes again immediately. Errors that are not the model's business
    /// -- a poisoned lock, a vanished plan -- belong to the caller, not here.
    async fn run(&self, input: Value, shop: &Workshop) -> DeedOutcome;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workshop() -> Workshop {
        Workshop::new(Workspace::in_place("/dev/city"))
    }

    /// The invariant the whole tool surface rests on. It is tested here, at the
    /// seam, because this is the only place it is enforced -- every tool gets it
    /// by construction, and a tool that could opt out would make it worthless.
    ///
    /// Traversal is the case worth pinning: `..` inside an otherwise innocent
    /// relative path is what an escape actually looks like, and resolving it
    /// lexically here is what stops the filesystem resolving it later.
    #[test]
    fn a_path_that_leaves_the_workspace_is_refused() {
        let shop = workshop();

        for escape in [
            "../secrets",
            "src/../../secrets",
            "/etc/passwd",
            "src/./../../..",
        ] {
            assert!(
                matches!(
                    shop.resolve(escape),
                    Err(Refusal::OutsideWorkspace { .. })
                ),
                "{escape:?} leaves the workspace and must be refused"
            );
        }
    }

    /// The other half: the boundary must not be so eager that ordinary work is
    /// impossible. A relative path is workspace-relative, an absolute path
    /// already inside is fine, and a `..` that stays within bounds is a legal
    /// way to name a sibling.
    #[test]
    fn ordinary_paths_resolve_inside_the_workspace() {
        let shop = workshop();

        assert_eq!(
            shop.resolve("src/main.rs").unwrap(),
            PathBuf::from("/dev/city/src/main.rs")
        );
        assert_eq!(
            shop.resolve("/dev/city/src/main.rs").unwrap(),
            PathBuf::from("/dev/city/src/main.rs")
        );
        assert_eq!(
            shop.resolve("src/../tests/it.rs").unwrap(),
            PathBuf::from("/dev/city/tests/it.rs"),
            "`..` that stays inside is ordinary, not an escape"
        );
    }

    /// A sibling directory sharing the workspace's name as a prefix is *not*
    /// inside it. Path prefix checks done on strings get this wrong, and the
    /// failure is silent: `/dev/city-old` would be writable by a plan that
    /// believes it is confined to `/dev/city`.
    #[test]
    fn a_sibling_with_a_shared_prefix_is_not_inside() {
        assert!(matches!(
            workshop().resolve("/dev/city-old/secrets"),
            Err(Refusal::OutsideWorkspace { .. })
        ));
    }
}
