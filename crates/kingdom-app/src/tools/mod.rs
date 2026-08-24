//! Tools: what the model can do with its own hands.
//!
//! Server-only, for the same reason `llm/` is. Named for what it is rather than
//! given a metaphor noun -- the metaphor is carried by
//! [`kingdom_core::ToolCall`], and inventing a second name for the plumbing
//! would only obscure where the subprocess is spawned. The precedent is set at
//! the top of `llm/mod.rs`.
//!
//! # The boundary
//!
//! Every tool is rooted at the plan's workspace, and [`Sandbox`] is the only
//! thing that knows where that is. A tool receives paths already resolved and
//! checked; it cannot reach outside because it is never told how to.
//!
//! That check lives here, at the seam, rather than in each tool. A check every
//! tool has to remember is a check the next tool will forget, and the tool that
//! forgets is the one that quietly edits a file in somebody else's checkout.
//!
//! Read [`Sandbox::resolve`] before adding a tool that touches the filesystem,
//! and [`Sandbox::root`] before adding one that spawns a process -- the
//! guarantee is real for the first and deliberately weaker for the second.

pub mod ask_user_question;
pub mod bash;
pub mod browser;
pub mod patch;
pub mod profile;
pub mod propose_plan;
pub mod read_file;
pub mod read_image;
pub mod search;
pub mod spawn_agents;
pub mod think;
pub mod tmux;

use kingdom_core::{ToolOutcome, Workspace};
use serde_json::Value;
use std::path::{Component, Path, PathBuf};

// `Permissions` moved down into the domain -- it crosses the wire now, because
// the conversation view renders differently while a plan is only proposing.
// Re-exported at its historical path so every existing `tools::Permissions`
// keeps resolving (move-down, re-export-up).
pub use kingdom_core::Permissions;

/// Every tool available under a given permission level.
///
/// A plain list rather than a registry with registration macros, for the same
/// reason `llm::providers()` is: a literal is the version of this a reader can
/// take in at a glance, and a tool that is not in this list does not exist.
/// Order is the order the model is shown them in.
///
/// This is the *only* definition of what a permission level permits. Both the
/// list a model is shown ([`crate::llm::ToolSpec::all`]) and the list it may
/// actually run ([`invoke`]) come through here, which is what stops the two
/// disagreeing -- a model that invents `bash` under read-only permissions must
/// not be handed `bash`.
///
/// # Why `Propose` keeps `bash` but loses `patch`
///
/// These look inconsistent and are not. This list is **not a sandbox** -- see
/// [`Sandbox::root`], which says plainly that the path boundary does not
/// contain a shell. Withholding `bash` from a proposing plan would therefore
/// buy a guarantee Kingdom cannot keep, while costing the model `git log`,
/// `cargo tree`, and running the failing test it is proposing to fix.
///
/// What the list *is* is a statement of the job. Offering `patch` says "you may
/// edit"; withholding it says "you may not", and the system prompt says the
/// rest in words. A model that means to follow its instructions is told clearly
/// what they are; one that does not was never going to be stopped by a missing
/// tool while it holds a shell.
pub fn all(permissions: Permissions) -> Vec<Box<dyn Tool>> {
    // Reads: everything, at every level. Looking at a picture is a read, so it
    // sits with the other reads rather than with the browser tools it was built
    // to pair with -- a subagent surveying a project may legitimately want to
    // look at a screenshot somebody already took; it still cannot take one.
    let mut tools: Vec<Box<dyn Tool>> = vec![
        Box::new(think::Think),
        Box::new(read_file::ReadFile),
        Box::new(search::Search),
        Box::new(read_image::ReadImage),
    ];

    // Acting on the world without changing the project: Propose and above.
    if matches!(permissions, Permissions::Propose | Permissions::Full) {
        tools.extend::<Vec<Box<dyn Tool>>>(vec![
            Box::new(bash::Bash),
            Box::new(tmux::TmuxRun),
            Box::new(tmux::Tmux),
            Box::new(browser::BrowserNavigate),
            Box::new(browser::BrowserClick),
            Box::new(browser::BrowserType),
            Box::new(browser::BrowserKeyPress),
            Box::new(browser::BrowserEval),
            Box::new(browser::BrowserWaitForSelector),
            Box::new(browser::BrowserTakeScreenshot),
            Box::new(browser::BrowserResize),
            Box::new(browser::BrowserRecentConsoleLogs),
            Box::new(browser::BrowserClearConsoleLogs),
            // Driving a browser at all is more than read-only, and profiling
            // drives one -- it navigates, clicks and throttles a real page.
            Box::new(profile::BrowserProfile),
            Box::new(ask_user_question::AskUserQuestion),
        ]);
    }

    // Putting a plan to the user: only while proposing. A plan with full
    // permissions is already carrying out a proposal they accepted and cannot
    // propose its way to more authority; a subagent answers to the plan that
    // sent it, and nothing about it is ever waiting on the user.
    if matches!(permissions, Permissions::Propose) {
        tools.push(Box::new(propose_plan::ProposePlan));
    }

    // Changing the project: only once the user has said so.
    if matches!(permissions, Permissions::Full) {
        tools.extend::<Vec<Box<dyn Tool>>>(vec![
            Box::new(patch::Patch),
            // Withheld while proposing for a duller reason than `patch`: a
            // subagent of a proposing plan is a case nobody has needed yet. It
            // would inherit `ReadOnly`, which is probably right -- but guessing
            // at an unasked-for shape is how the lease machinery happened.
            Box::new(spawn_agents::SpawnAgents),
        ]);
    }

    tools
}

/// Runs one tool call by name, inside the workspace's bounds and its
/// permissions.
///
/// The single point where a name from a model becomes work on a real machine.
/// An unknown name is a refusal reported back to the model rather than an
/// error: models hallucinate tool names, and being told "there is no such tool"
/// is something a model can recover from in one turn.
///
/// A tool that exists but is outside the current permissions gets a *different*
/// answer, and the difference earns its keep. "There is no such tool" is true
/// from where a model stands -- it was never shown the tool -- but it is a dead
/// end: the obvious recovery is to give up on the whole approach. Saying that
/// the tool exists and is not available *yet* points at the actual next move,
/// which for a proposing plan is to put a plan to the user.
pub async fn invoke(tool: &str, input: Value, shop: &Sandbox) -> ToolOutcome {
    if let Some(t) = all(shop.permissions()).into_iter().find(|t| t.name() == tool) {
        return t.run(input, shop).await;
    }

    // Known to Kingdom, but not at this level. Worth distinguishing, because
    // the model is not wrong about the tool existing -- only about what it may
    // do right now.
    let exists_elsewhere = [
        Permissions::ReadOnly,
        Permissions::Propose,
        Permissions::Full,
    ]
    .into_iter()
    .any(|p| all(p).iter().any(|t| t.name() == tool));

    match (exists_elsewhere, shop.permissions()) {
        (true, Permissions::Propose) => Refusal::Refused(format!(
            "`{tool}` is not available while you are drawing up a plan. Put the plan to \
             the user with `propose_plan`; if they start you on it, you will have it."
        ))
        .into(),
        (true, Permissions::ReadOnly) => Refusal::Refused(format!(
            "`{tool}` is not available to a subagent. You were sent to read and report, \
             so report what you found to the plan that sent you."
        ))
        .into(),
        _ => Refusal::NoSuchTool(tool.to_string()).into(),
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
/// to put a question in front of the user and wait for his answer -- needs
/// something the browser can name when it answers, and the tool call it is
/// already being rendered as is exactly that. Minting a second identifier would
/// mean keeping two in step for no gain.
#[derive(Debug, Clone)]
pub struct Sandbox {
    workspace: Workspace,
    plan: kingdom_core::PlanId,
    /// The tool call this call is recorded as, once it is running. `None`
    /// outside a turn, which is the case tests construct.
    tool_call: Option<String>,
    /// How much of the world this plan may touch. See [`Permissions`].
    permissions: Permissions,
}

/// A refusal: the tool would not run, and why.
///
/// The reason is written for the *model*, not for the user. It is fed back as
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

impl From<Refusal> for ToolOutcome {
    fn from(refusal: Refusal) -> Self {
        ToolOutcome::Refused {
            reason: refusal.to_string(),
        }
    }
}

impl Sandbox {
    /// A sandbox with the model's full hands, which is what a plan the user
    /// opened gets.
    pub fn new(workspace: Workspace) -> Self {
        Self {
            workspace,
            plan: kingdom_core::PlanId::new(String::new()),
            tool_call: None,
            permissions: Permissions::Full,
        }
    }

    /// Narrows what this sandbox may do. See [`Permissions`].
    pub fn under(mut self, permissions: Permissions) -> Self {
        self.permissions = permissions;
        self
    }

    /// How much of the world this plan may touch.
    pub fn permissions(&self) -> Permissions {
        self.permissions
    }

    /// Names the plan these tools work for.
    pub fn for_plan(mut self, plan: kingdom_core::PlanId) -> Self {
        self.plan = plan;
        self
    }

    /// A copy of this sandbox bound to one recorded call.
    ///
    /// Cloned per call rather than mutated, so a tool cannot see the id of a
    /// call that is not its own -- which is what would let one tool answer
    /// another's question.
    pub fn for_tool_call(&self, tool_call: impl Into<String>) -> Self {
        Self {
            tool_call: Some(tool_call.into()),
            ..self.clone()
        }
    }

    /// Which plan this call belongs to.
    pub fn plan(&self) -> &kingdom_core::PlanId {
        &self.plan
    }

    /// The tool call this call is recorded as.
    pub fn tool_call(&self) -> Option<&str> {
        self.tool_call.as_deref()
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
    /// False means the user chose to work directly in the city, so the boundary
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

/// One thing the model can do.
///
/// Stateless: every tool is a singleton and all per-call context arrives in the
/// [`Sandbox`]. That is what lets one instance serve every plan at once
/// without a tool ever holding a plan's path in a field, which is the mistake
/// that would make the boundary above a suggestion.
#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    /// The name the model calls it by, and the name recorded on the tool call.
    fn name(&self) -> &'static str;

    /// What it does, written for a model.
    fn description(&self) -> String;

    /// JSON Schema for the arguments.
    fn input_schema(&self) -> Value;

    /// Runs the tool.
    ///
    /// Returns a [`ToolOutcome`] rather than a `Result` because both endings
    /// are results the model must be *told*: a refusal it never hears about is
    /// a call it makes again immediately. Errors that are not the model's
    /// business -- a poisoned lock, a vanished plan -- belong to the caller,
    /// not here.
    async fn run(&self, input: Value, shop: &Sandbox) -> ToolOutcome;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sandbox() -> Sandbox {
        Sandbox::new(Workspace::in_place("/dev/city"))
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
        let shop = sandbox();

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
        let shop = sandbox();

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
            sandbox().resolve("/dev/city-old/secrets"),
            Err(Refusal::OutsideWorkspace { .. })
        ));
    }

    /// The invariant subagents rest on, and the level that was added beside it.
    ///
    /// Subagents run in parallel inside their parent's worktree, which is only
    /// safe because they cannot write. That is enforced here, at the seam, and
    /// nowhere else -- so this is where it is worth pinning, beside the path
    /// check it sits next to for the same reason.
    ///
    /// The refusal matters as much as the absence: a model under read-only
    /// permissions was never shown `bash`, but models invent tool names, and
    /// one that invents this one must not be handed it.
    #[tokio::test]
    async fn a_survey_cannot_reach_the_tools_that_touch_the_world() {
        let surveying = sandbox().under(Permissions::ReadOnly);

        for forbidden in ["bash", "patch", "tmux_run", "browser_navigate", "spawn_agents"] {
            assert!(
                !all(Permissions::ReadOnly).iter().any(|t| t.name() == forbidden),
                "{forbidden} must not be offered to a survey"
            );
            assert!(
                matches!(
                    invoke(forbidden, serde_json::json!({}), &surveying).await,
                    ToolOutcome::Refused { .. }
                ),
                "{forbidden} must be refused even when a survey asks for it by name"
            );
        }

        // The other half: a survey is not crippled. It exists to read and
        // report, and it must still be able to.
        for allowed in ["think", "read_file", "search"] {
            assert!(
                all(Permissions::ReadOnly).iter().any(|t| t.name() == allowed),
                "{allowed} is how a survey does its job"
            );
        }
    }

    /// The Propose level: what a plan drawing one up may and may not do.
    ///
    /// The withheld half is the load-bearing one, and it is narrower than it
    /// looks. Propose keeps `bash` deliberately -- see the note on [`all`] --
    /// so `patch` is the single tool standing between a proposing model and
    /// editing the project. If it ever leaks into this level, every prompt
    /// silently goes back to changing files before the user has seen a plan,
    /// and nothing else in the system would notice.
    ///
    /// The refusal is tested as well as the absence for the same reason as
    /// above: the list a model is *shown* and the list it may *run* must not
    /// disagree, and a model that invents `patch` must not be handed it.
    #[tokio::test]
    async fn proposing_may_look_and_run_but_not_change_the_project() {
        let proposing = sandbox().under(Permissions::Propose);

        for forbidden in ["patch", "spawn_agents"] {
            assert!(
                !all(Permissions::Propose).iter().any(|t| t.name() == forbidden),
                "{forbidden} must not be offered while drawing up a plan"
            );
            assert!(
                matches!(
                    invoke(forbidden, serde_json::json!({}), &proposing).await,
                    ToolOutcome::Refused { .. }
                ),
                "{forbidden} must be refused even when a proposing plan asks by name"
            );
        }

        // Proposing is meant to be a *strong* explorer: it can run the failing
        // test it is proposing to fix, and look at the app it is changing.
        for allowed in [
            "think",
            "read_file",
            "search",
            "bash",
            "browser_navigate",
            "propose_plan",
        ] {
            assert!(
                all(Permissions::Propose).iter().any(|t| t.name() == allowed),
                "{allowed} is how a proposing plan does its job"
            );
        }

        // `propose_plan` is the one tool that belongs to this level *alone*: a
        // plan already working has nothing left to be granted, and a subagent
        // reports to the plan that sent it rather than to the user.
        for other in [Permissions::ReadOnly, Permissions::Full] {
            assert!(
                !all(other).iter().any(|t| t.name() == "propose_plan"),
                "only a proposing plan puts work to the user"
            );
        }
    }
}
