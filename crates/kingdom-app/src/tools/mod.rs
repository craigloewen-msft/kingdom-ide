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
pub mod skill;
pub mod spawn_agents;
pub mod think;
pub mod tmux;

use kingdom_core::{ToolOutcome, WaitBudget, Workspace};
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
/// # Why `Propose` writes only its own draft
///
/// A proposing plan gets `patch`, scoped to one markdown file
/// ([`patch::Patch::for_draft`], [`propose_plan::DRAFT`]). It cannot touch the
/// project. This is Phoenix's shape: its Explore mode carries a `patch` scoped
/// to the tasks directory for exactly the same reason.
///
/// The reason is not containment. This list is **not a sandbox** -- see
/// [`Sandbox::root`], which says plainly that the path boundary does not contain
/// a shell, and a proposing plan holds `bash`. Withholding tools here buys no
/// guarantee Kingdom can keep.
///
/// What the scoped patch buys is somewhere for the model to *put the plan* while
/// it works it out. Without it the plan has to be produced whole, from memory,
/// in a single `propose_plan` call -- and the observed result was a plan that
/// investigated for 21 rounds, re-deciding the same names over and over, and
/// never proposed at all. `propose_plan`'s module docs carry that story in full.
///
/// So the list still states the job, as it always did; only the sentence
/// changed. Offering the editing tool *unrestricted* says "you may change the
/// project"; offering it scoped to a draft says "you may write down what you
/// would change". The system prompt says the rest in words.
pub fn all(permissions: Permissions) -> Vec<Box<dyn Tool>> {
    // Reads: everything, at every level. Looking at a picture is a read, so it
    // sits with the other reads rather than with the browser tools it was built
    // to pair with -- a subagent surveying a project may legitimately want to
    // look at a screenshot somebody already took; it still cannot take one.
    //
    // `skill` is here for the same reason: invoking one returns instructions and
    // changes nothing. Whether those instructions can then be *carried out* is
    // decided by the rest of this list, which is the boundary that already
    // exists -- so a proposing plan may read a deployment skill and still not be
    // able to deploy.
    let mut tools: Vec<Box<dyn Tool>> = vec![
        Box::new(think::Think),
        Box::new(read_file::ReadFile),
        Box::new(search::Search),
        Box::new(read_image::ReadImage),
        Box::new(skill::Skill),
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
    //
    // The scoped `patch` arrives with it, because the two are one mechanism:
    // the draft is what `propose_plan` names, and the write is what points the
    // model back at `propose_plan`.
    if matches!(permissions, Permissions::Propose) {
        tools.push(Box::new(patch::Patch::for_draft(propose_plan::DRAFT)));
        tools.push(Box::new(propose_plan::ProposePlan));
    }

    // Changing the project: only once the user has said so.
    if matches!(permissions, Permissions::Full) {
        tools.extend::<Vec<Box<dyn Tool>>>(vec![
            Box::new(patch::Patch::unrestricted()),
            // Withheld while proposing for a duller reason than `patch`: a
            // subagent of a proposing plan is a case nobody has needed yet. It
            // would inherit `ReadOnly`, which is probably right -- but guessing
            // at an unasked-for shape is how the lease machinery happened.
            Box::new(spawn_agents::SpawnAgents),
        ]);
    }

    tools
}

/// How long a call to `tool` will wait, before it is run.
///
/// The counterpart to [`invoke`], and separate from it because the answer is
/// wanted at a different moment: the call is *recorded* before it runs, so the
/// chamber can show a command while it is still going, and the budget has to be
/// on it at that point or the King watches a figure appear only once it no
/// longer matters.
///
/// An unknown tool, or one outside these permissions, waits for nothing as far
/// as anyone can tell: it is about to be refused by [`invoke`] and never waits
/// at all.
pub fn waits_for(tool: &str, input: &Value, shop: &Sandbox) -> Option<WaitBudget> {
    all(shop.permissions())
        .into_iter()
        .find(|t| t.name() == tool)
        .and_then(|t| t.waits_for(input))
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
    if let Some(t) = all(shop.permissions())
        .into_iter()
        .find(|t| t.name() == tool)
    {
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
    /// Whether the model driving this turn can look at a picture.
    ///
    /// The third narrowing, beside [`Permissions`] and `Model::can_act`, and the
    /// only one a *tool* needs at run time rather than at offer time.
    /// `ToolSpec::for_model` handles the others by withholding the tool
    /// entirely, which does not work here: `browser_take_screenshot` is worth
    /// offering to a blind model -- the King still sees the picture in the
    /// chamber -- it simply must not be handed the base64.
    ///
    /// Defaults to true, so a caller that never says is assumed sighted. The
    /// cost of being wrong that way is bytes; the other way it is a model told
    /// nothing about an image it could have read.
    sighted: bool,
}

/// A refusal: the tool would not run, and why.
///
/// The reason is written for the *model*, not for the user. It is fed back as
/// the call's result, so it has to be actionable -- "that path is outside the
/// workspace" lets a model correct itself, where a bare "refused" earns a retry
/// of exactly the same call.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    #[error(
        "{path} is outside this plan's workspace. Everything this plan may \
             touch is under {root}; use a path inside it."
    )]
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
            sighted: true,
        }
    }

    /// Records whether the model driving this turn can look at a picture.
    ///
    /// See the field. The one caller is `api.rs`, which asks the model the same
    /// question [`crate::llm::ToolSpec::for_model`] asks it.
    pub fn seen_by_a_sighted_model(mut self, sighted: bool) -> Self {
        self.sighted = sighted;
        self
    }

    /// Whether a tool should bother handing this model an image.
    pub fn sighted(&self) -> bool {
        self.sighted
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

    /// The other direction: a resolved path as the workspace sees it.
    ///
    /// For recording rather than for opening. A [`kingdom_core::ToolArtifact`]
    /// names a file relative to the workspace, so the record does not carry a
    /// fact about one machine and a viewer has something it can hand back
    /// through [`Sandbox::resolve`] -- which is what keeps the boundary in one
    /// place rather than two.
    ///
    /// `None` for a path outside the workspace, which the caller should read as
    /// "nothing to record" rather than as an error: the tool has already done
    /// its work, and a file with no serveable path is simply one the chamber
    /// cannot show.
    pub fn relative(&self, path: &Path) -> Option<String> {
        Some(
            normalise(path)
                .strip_prefix(normalise(self.root()))
                .ok()?
                .to_string_lossy()
                .into_owned(),
        )
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

/// The environment a plan's child processes run under, beyond what this
/// server inherited.
///
/// # The city's shared services
///
/// A project that declares a well in `<city>/.kingdom/services.toml` has its
/// address handed to every command the plan runs, under the names the manifest
/// chose -- `MONGODB_URI` and the like. This is **the** way a plan finds the
/// database, because a plan's namespace cannot resolve Docker's service names;
/// an address is the only thing that works, and it is not knowable until the
/// container exists.
///
/// It is applied here rather than in each tool for the reason
/// `netns::enter_prefix` is applied there: `bash`, `tmux` and the King's own
/// terminal already route through this one function, and a call site that had
/// to *remember* to add the database address is one that will forget.
///
/// # A Kingdom checking out Kingdom
///
/// Empty for an ordinary project: a plan working on somebody else's repository
/// gets the machine as it is, and imposing variables on it would be Kingdom
/// reaching into work that is none of its business.
///
/// It is *not* empty when the workspace is a Kingdom checkout, because that is
/// the one case where a child process is another Kingdom -- a rehearsal server
/// the plan starts to look at its own work -- and the two collide in ways only
/// this side can see:
///
/// - **`KINGDOM_HOME`.** Unset, `profile::home()` resolves to the King's real
///   `~/.kingdom`, so a rehearsal writes its throwaway plan records into the
///   King's own drawer. Pointed inside the workspace they arrive and depart
///   with it. This is the resource collision this product exists to surface,
///   happening inside the product.
/// - **`KINGDOM_MODEL`.** The child inherits whatever credential this server
///   has, so its picker opens on a *recommended* paid model and a plan testing
///   a button spends the King's quota to do it. `mock` is the honest default
///   for a rehearsal. A default rather than a restriction -- the picker still
///   offers everything, and a plan that genuinely needs a real model can
///   choose one or override this in the command it runs.
///
/// None of it is forced: these are applied to the child before it runs, so a
/// command that names its own `MONGODB_URI` inline still wins.
pub fn child_environment(shop: &Sandbox) -> Vec<(String, String)> {
    // The city's shared services first, so the Kingdom-specific pair below is the
    // one that survives a duplicate key -- a rehearsal server's `KINGDOM_HOME`
    // is Kingdom's own business and a manifest must not be able to move it.
    let mut environment = service_environment(shop);

    if !runs_a_kingdom(shop.root()) {
        return environment;
    }
    environment.extend([
        (
            crate::profile::HOME_VAR.to_string(),
            shop.root()
                .join(".kingdom")
                .join("profile")
                .display()
                .to_string(),
        ),
        ("KINGDOM_MODEL".to_string(), "mock".to_string()),
    ]);
    environment
}

/// The address of every service this plan's city has standing.
///
/// Read from the running registry rather than from the manifest alone: a
/// service that is declared but not up has no address, and inventing one would
/// hand the plan a URI that fails to connect for no visible reason.
///
/// The city is resolved through `api::city_root_of` rather than from the
/// sandbox's own path, because the sandbox points at the plan's *worktree* and
/// the well belongs to the project. Five worktrees, one city, one address.
fn service_environment(shop: &Sandbox) -> Vec<(String, String)> {
    let Some(city_root) = crate::api::city_root_of(shop.plan()) else {
        return Vec::new();
    };
    crate::services::environment(&city_root)
}

/// Whether this workspace is a checkout of Kingdom itself.
///
/// Asked of a file only Kingdom has, rather than of the directory's name: a
/// worktree is named for a plan id, and a fork or a rename would answer wrongly
/// either way.
///
/// Public because two things need the same answer and must not disagree about
/// it: [`child_environment`], which points a rehearsal server at the mock, and
/// the system prompt, which tells the plan that has been done for it.
pub fn runs_a_kingdom(root: &Path) -> bool {
    root.join("crates")
        .join("kingdom-app")
        .join("Cargo.toml")
        .is_file()
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

    /// How long this call will wait before it stops waiting, read from the
    /// arguments the model sent.
    ///
    /// The chamber puts this on the deed's line, so the King can tell a build
    /// that has forty seconds left from a browser call that ran out thirty
    /// seconds ago. `None` -- the default, and the answer for most tools -- is
    /// "this does not wait on anything", and the line simply shows how long it
    /// has been going.
    ///
    /// **It lives on the tool because the numbers do.** Every default here is a
    /// constant inside the tool that parses it, and the arguments are the
    /// model's own JSON. A table of these kept next to the view would be a
    /// second copy of the tool surface: wrong the first time anybody changed a
    /// default, and wrong silently, since a figure on a line looks equally
    /// confident either way.
    ///
    /// Answered from the arguments alone, never by starting anything. This is
    /// called while the call is being *recorded*, before [`Tool::run`].
    fn waits_for(&self, _input: &Value) -> Option<WaitBudget> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sandbox() -> Sandbox {
        Sandbox::new(Workspace::in_place("/dev/city"))
    }

    /// An ordinary project is handed the machine as it is.
    ///
    /// Kingdom imposing variables on a repository that is not its own would be
    /// reaching into work that is none of its business, and would be invisible
    /// to whoever later wondered why their build behaved differently here.
    #[test]
    fn a_project_that_is_not_kingdom_gets_no_environment_of_ours() {
        let dir = tempfile::tempdir().expect("a temporary workspace");
        let shop = Sandbox::new(Workspace::in_place(dir.path().display().to_string()));

        assert!(child_environment(&shop).is_empty());
    }

    /// A Kingdom checkout gets both, and `KINGDOM_HOME` lands *inside* the
    /// workspace.
    ///
    /// That last part is the point of the variable rather than a detail: left
    /// unset the child resolves the King's real `~/.kingdom` and a rehearsal
    /// writes throwaway plan records into his own drawer. A value outside the
    /// workspace would be the same fault with extra steps.
    #[test]
    fn a_kingdom_checkout_is_pointed_at_the_mock_and_its_own_profile() {
        let dir = tempfile::tempdir().expect("a temporary workspace");
        let crates = dir.path().join("crates").join("kingdom-app");
        std::fs::create_dir_all(&crates).expect("the marker's directory");
        std::fs::write(crates.join("Cargo.toml"), "[package]\n").expect("the marker");

        let shop = Sandbox::new(Workspace::in_place(dir.path().display().to_string()));
        let environment = child_environment(&shop);

        let home = environment
            .iter()
            .find(|(key, _)| key == crate::profile::HOME_VAR)
            .map(|(_, value)| value.clone())
            .expect("a rehearsal keeps its own records");
        assert!(
            Path::new(&home).starts_with(dir.path()),
            "{home} is not inside the workspace"
        );

        assert!(
            environment
                .iter()
                .any(|(key, value)| key == "KINGDOM_MODEL" && value == "mock"),
            "a rehearsal must not open on a model that costs the King money"
        );
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
                matches!(shop.resolve(escape), Err(Refusal::OutsideWorkspace { .. })),
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

    /// The round trip a recorded artifact depends on: what `relative` names,
    /// `resolve` must find again.
    ///
    /// These are the two ends of one path -- a tool writes a file and records
    /// where it put it, and later the artifact route is handed that record and
    /// has to open the same file. If they ever disagree the symptom is a
    /// picture that 403s in a chamber, with nothing in either function looking
    /// wrong on its own.
    #[test]
    fn what_the_workspace_names_it_can_find_again() {
        let shop = sandbox();
        let written = shop.resolve("shots/a.png").unwrap();

        let recorded = shop.relative(&written).expect("a path inside is nameable");
        assert_eq!(recorded, "shots/a.png", "records must not carry the root");
        assert_eq!(
            shop.resolve(&recorded).unwrap(),
            written,
            "a recorded path must resolve back to the file it named"
        );

        assert!(
            shop.relative(Path::new("/dev/elsewhere/a.png")).is_none(),
            "a file outside the workspace has no name this plan can serve"
        );
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

        for forbidden in [
            "bash",
            "patch",
            "tmux_run",
            "browser_navigate",
            "spawn_agents",
        ] {
            assert!(
                !all(Permissions::ReadOnly)
                    .iter()
                    .any(|t| t.name() == forbidden),
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
                all(Permissions::ReadOnly)
                    .iter()
                    .any(|t| t.name() == allowed),
                "{allowed} is how a survey does its job"
            );
        }
    }

    /// The Propose level: what a plan drawing one up may and may not do.
    ///
    /// The load-bearing claim moved when `patch` did. It used to be that
    /// `patch` was absent entirely, so its mere presence was the alarm. Now it
    /// is present and *scoped*, and the thing that must not leak is the
    /// scope: a proposing plan may write its own draft and nothing else.
    ///
    /// So this pins the boundary where it actually lives -- by calling the tool
    /// and checking the project is unchanged -- rather than by the tool's name.
    /// A scope that silently widened would put every prompt back to editing
    /// files before the user has seen a plan, and nothing else in the system
    /// would notice.
    ///
    /// The refusal is tested as well as the absence for the same reason as
    /// above: the list a model is *shown* and the list it may *run* must not
    /// disagree, and a model that invents `spawn_agents` must not be handed it.
    #[tokio::test]
    async fn proposing_may_look_and_run_but_not_change_the_project() {
        let proposing = sandbox().under(Permissions::Propose);

        for forbidden in ["spawn_agents"] {
            assert!(
                !all(Permissions::Propose)
                    .iter()
                    .any(|t| t.name() == forbidden),
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

        // `patch` *is* offered now, because the model needs somewhere to write
        // the plan down -- see the note on `all`. What must still hold is that
        // it cannot reach the project.
        assert!(
            all(Permissions::Propose)
                .iter()
                .any(|t| t.name() == "patch"),
            "a proposing plan drafts with `patch`; without it there is nowhere \
             to put the plan and it never stops investigating"
        );

        let root = tempfile::tempdir().unwrap();
        let writing = Sandbox::new(Workspace::in_place(root.path().to_str().unwrap()))
            .under(Permissions::Propose);
        std::fs::write(root.path().join("main.rs"), "fn main() {}\n").unwrap();

        let refused = invoke(
            "patch",
            serde_json::json!({
                "path": "main.rs",
                "patches": [{"operation": "overwrite", "newText": "owned\n"}]
            }),
            &writing,
        )
        .await;

        assert!(
            matches!(refused, ToolOutcome::Refused { .. }),
            "a proposing plan must not edit the project: {refused:?}"
        );
        assert_eq!(
            std::fs::read_to_string(root.path().join("main.rs")).unwrap(),
            "fn main() {}\n",
            "the file must be untouched"
        );

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
                all(Permissions::Propose)
                    .iter()
                    .any(|t| t.name() == allowed),
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

    /// The distinction the deed line's whole rendering rests on, checked at the
    /// two tools that sit on opposite sides of it.
    ///
    /// `bash` reaching `wait_seconds` is the design working -- the command runs
    /// on and a handle comes back -- while `browser_click` reaching its timeout
    /// has failed outright. The chamber colours one and not the other, so an
    /// inverted variant here would either cry alarm over every cold build or go
    /// quiet on the calls actually going wrong. Neither looks broken on screen,
    /// which is why it is pinned rather than left to reading.
    #[test]
    fn a_shell_asks_for_patience_and_a_browser_sets_a_deadline() {
        let shop = sandbox();

        assert_eq!(
            waits_for("bash", &serde_json::json!({ "cmd": "cargo build" }), &shop),
            Some(WaitBudget::Patience { seconds: 30 }),
            "nothing is killed when a shell's wait elapses, so it is never a deadline"
        );
        assert_eq!(
            waits_for(
                "browser_click",
                &serde_json::json!({ "selector": ".btn" }),
                &shop
            ),
            Some(WaitBudget::Deadline { seconds: 30 }),
            "a browser call that runs out of time has failed, and nothing is left running"
        );
    }

    /// The model's own figure wins where it gave one, and each tool's own
    /// default stands in where it did not.
    ///
    /// The defaults differ per tool and per *operation* -- 15s to act on a page
    /// that is there, 30s to wait for one to arrive -- and the point of asking
    /// the tool rather than a table is that these stay true when somebody edits
    /// them. This is the test that fails when the two copies drift.
    #[test]
    fn a_wait_is_the_models_own_where_it_gave_one_and_the_tools_default_otherwise() {
        let shop = sandbox();

        assert_eq!(
            waits_for(
                "bash",
                &serde_json::json!({ "cmd": "sleep 100", "wait_seconds": 0 }),
                &shop
            ),
            Some(WaitBudget::Patience { seconds: 0 }),
            "asking not to wait at all is a wait of zero, not an absent budget"
        );
        assert_eq!(
            waits_for(
                "browser_navigate",
                &serde_json::json!({ "url": "http://localhost", "timeout": "2m" }),
                &shop
            ),
            Some(WaitBudget::Deadline { seconds: 120 }),
            "a timeout is read in the tool's own vocabulary, so `2m` is two minutes"
        );
        assert_eq!(
            waits_for(
                "browser_navigate",
                &serde_json::json!({ "url": "http://localhost" }),
                &shop
            ),
            Some(WaitBudget::Deadline { seconds: 15 }),
            "acting on a page that is already there gets the shorter default"
        );
        assert_eq!(
            waits_for(
                "browser_wait_for_selector",
                &serde_json::json!({ "selector": "#app" }),
                &shop
            ),
            Some(WaitBudget::Deadline { seconds: 30 }),
            "waiting for a page to become something gets the longer one"
        );
    }

    /// A budget is a claim about waiting, so the tools that do not wait must not
    /// make one. Silence on the line means "this simply takes as long as it
    /// takes", and a figure there would have the King watching a countdown that
    /// governs nothing.
    #[test]
    fn a_tool_that_does_not_wait_says_nothing() {
        let shop = sandbox();

        assert_eq!(
            waits_for(
                "read_file",
                &serde_json::json!({ "path": "src/lib.rs" }),
                &shop
            ),
            None
        );
        assert_eq!(
            waits_for(
                "bash",
                &serde_json::json!({ "op": "peek", "handle": "b-1" }),
                &shop
            ),
            None,
            "a peek answers from what is already known and returns at once"
        );
        assert_eq!(
            waits_for(
                "tmux_run",
                &serde_json::json!({ "cmd": "npm run dev" }),
                &shop
            ),
            None,
            "without `readiness` a tmux window is opened and not waited on"
        );
        assert_eq!(
            waits_for(
                "tmux_run",
                &serde_json::json!({
                    "cmd": "npm run dev",
                    "readiness": { "text": "listening on", "timeout_seconds": 45 }
                }),
                &shop
            ),
            Some(WaitBudget::Patience { seconds: 45 }),
            "and with it, the command still outlives the watching -- so, patience"
        );

        // Not a tool at all: about to be refused, and it never waits.
        assert_eq!(waits_for("nonesuch", &serde_json::json!({}), &shop), None);
    }
}
