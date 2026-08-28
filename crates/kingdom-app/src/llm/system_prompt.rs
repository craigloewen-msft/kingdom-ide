//! The system prompt: everything the model is told before it is asked anything.
//!
//! Server-only, and lifted out of the provider on purpose. This used to be
//! built inside `copilot.rs`, which made it *Copilot's* prompt: a second
//! provider would have had to reinvent it, and the two would have drifted the
//! first time either was touched. What a model is told about the work is
//! content, not transport, so it is assembled once here and every provider
//! renders the same words.
//!
//! # Ported from Phoenix IDE
//!
//! The wording and the block order are Phoenix's
//! (`crates/phoenix-ide/src/system_prompt.rs` and the `PhoenixNative` arms of
//! `crates/phoenix-core/src/llm_language.rs`), because its agents demonstrably
//! work better. Kingdom previously carried several blocks of its own -- advice
//! on ending a turn, on the cost of re-reading, on writing tests, on
//! screenshots -- each written in response to a real failure. They are gone.
//! Phoenix sends none of them and does better regardless, and a prompt that is
//! *nearly* Phoenix's plus four house paragraphs is neither.
//!
//! **Phoenix wins on wording, never on facts about Kingdom.**
//! [`shared_machine_block`] has no Phoenix counterpart and is kept anyway,
//! because sharing one machine between agents is Kingdom's own subject.
//! [`MERMAID`] is the other way round:
//! it is Phoenix's sentence, deleted from Kingdom for years because it was false
//! here, and restored the day `components/markdown.rs` made it true. A
//! description is a promise about behaviour; matching prose that promises
//! something Kingdom does not do is the one way this port can make the agents
//! worse rather than better.
//!
//! **The order carries the reasoning.** The remit lands last, after project
//! guidance, because it is what the model must be holding when it starts work.
//! Kingdom used to put it near the top and then bury it under up to 64 KB of
//! `AGENTS.md`. Adding anything after the mode block puts that same distance
//! back.

use super::CityBrief;
use crate::skills::Skill;
use kingdom_core::Permissions;
use std::collections::HashSet;
use std::fmt::Write;
use std::path::{Path, PathBuf};

/// Guidance filenames, in order of preference within one directory.
const GUIDANCE_NAMES: &[&str] = &["AGENTS.md", "AGENT.md"];

/// How much project guidance is worth sending.
///
/// A cap rather than trust, because the cost is per *round*, not per turn: the
/// whole prompt is resent on every pass of a loop that may run 500 times, so a
/// megabyte of `AGENTS.md` is a bill rather than merely a long prompt. Generous
/// enough that no honest guidance file comes near it.
const MOST_GUIDANCE: usize = 64 * 1024;

/// Everything the model is told about the work, assembled once per turn.
///
/// Held as parts rather than one finished string so a provider can place them
/// as its own API prefers -- and so the pieces stay testable individually.
#[derive(Debug, Clone, Default)]
pub struct SystemPrompt {
    /// The project.
    ///
    /// **Not rendered into the prompt.** Phoenix sends no project summary and no
    /// file listing; Kingdom used to spend up to 40 paths on every round of
    /// every turn, which is a large recurring bill for something `search` answers
    /// on demand and more accurately. It is still carried because a provider
    /// occasionally needs a *fact* from it -- the city's name for a fallback
    /// headline -- and the offline mock builds its replies out of it.
    pub city: CityBrief,
    /// Where the model is standing, and whether it is isolated.
    pub workspace: String,
    /// What it may do, and what it must not. Rendered last. See the module note.
    pub permissions: String,
    /// Every `AGENTS.md` found on the way up from the workspace.
    pub guidance: Vec<Guidance>,
    /// Every skill the workspace can reach, as a catalogue of names and
    /// descriptions. Bodies are fetched on demand by the `skill` tool.
    pub skills: Vec<Skill>,
    /// That the machine has other tenants -- worded for how far this plan is
    /// walled off from them. See [`shared_machine_block`].
    pub shared_machine: String,
    /// The city's shared services and where **this plan** reaches them, or
    /// empty when it declares none.
    ///
    /// Said in the prompt as well as put in the environment because the two
    /// answer different questions. The environment is what a command *uses*;
    /// this is what decides what the model writes in the first place -- and
    /// every model's prior is `localhost`. For an isolated plan that prior is
    /// now right, and this says so; for a plan on the machine's network it is
    /// still wrong, and this still warns.
    pub services: String,
}

/// One guidance file, and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Guidance {
    pub path: String,
    pub body: String,
}

impl SystemPrompt {
    /// Assembles the prompt for one turn.
    ///
    /// `kingdom_root` bounds the guidance walk. See [`discover_guidance`] for
    /// why that bound is load-bearing rather than tidiness.
    ///
    /// The `plan` is here for one reason: two plans on one project are not
    /// necessarily told the same address for its database. See
    /// [`services_block`].
    ///
    /// `city_root` is separate from `city.path` and must be: a
    /// [`CityBrief`]'s path is the **plan's workspace**, which for an isolated
    /// plan is a worktree under `.kingdom/`, while a shared resource belongs to
    /// the *project*. Passing the workspace here silently matched no city key,
    /// and the block came out empty -- so a plan whose project shares a
    /// database was never told it had one.
    // Nine independent facts, not a struct waiting to happen: `city_root` and
    // `city.path` differ on purpose (see above), so naming each at the call
    // site is what keeps them from being confused.
    #[allow(clippy::too_many_arguments)]
    pub fn assemble(
        plan: &kingdom_core::PlanId,
        city: &CityBrief,
        workspace: &kingdom_core::Workspace,
        permissions: Permissions,
        approved: bool,
        kingdom_root: &Path,
        city_root: &Path,
        isolation: kingdom_core::Isolation,
        allowed: Vec<kingdom_core::services::MountSpec>,
    ) -> Self {
        let root = Path::new(&workspace.path);
        Self {
            city: city.clone(),
            workspace: workspace_block(workspace, isolation, &allowed),
            permissions: permissions_block(permissions, approved),
            guidance: discover_guidance(root, kingdom_root),
            skills: crate::skills::discover(root),
            shared_machine: shared_machine_block(isolation),
            services: services_block(plan, city_root),
        }
    }

    /// The prompt as one system prompt.
    ///
    /// Phoenix's order exactly. The remit is last: it is the most specific
    /// instruction here and the one the model must still be holding when it
    /// picks its first tool. Anything appended after it re-creates the problem
    /// this order was adopted to fix.
    pub fn render(&self) -> String {
        let mut out = String::from(BASE);

        out.push_str("\n\n");
        out.push_str(MERMAID);

        out.push_str("\n\n");
        out.push_str(BATCHING);

        if !self.shared_machine.is_empty() {
            out.push_str("\n\n");
            out.push_str(&self.shared_machine);
        }

        if !self.guidance.is_empty() {
            out.push_str("\n\n<project_guidance>\n");
            for (i, file) in self.guidance.iter().enumerate() {
                if i > 0 {
                    out.push_str("\n---\n\n");
                }
                let _ = writeln!(out, "<!-- From: {} -->", file.path);
                out.push_str(&file.body);
                if !file.body.ends_with('\n') {
                    out.push('\n');
                }
            }
            out.push_str("</project_guidance>");
        }

        if !self.skills.is_empty() {
            out.push_str("\n\n<available_skills>\n");
            out.push_str(SKILLS_PREAMBLE);
            for skill in &self.skills {
                let _ = writeln!(
                    out,
                    "\n- **{}** — {} {}",
                    skill.name,
                    skill.description,
                    skill.display_location()
                );
            }
            out.push_str("</available_skills>");
        }

        if !self.workspace.is_empty() {
            out.push_str("\n\n");
            out.push_str(&self.workspace);
        }

        // After the workspace block, because it is a fact about where the model
        // is standing, and before the remit, which must stay last.
        if !self.services.is_empty() {
            out.push_str("\n\n");
            out.push_str(&self.services);
        }

        out.push_str("\n\n");
        out.push_str(self.permissions.trim_start());

        out
    }
}

/// Phoenix's base prompt, verbatim.
const BASE: &str = "You are a helpful AI assistant with access to tools for executing code, \
     editing files, and searching codebases. Use tools when appropriate to accomplish tasks.\n\n\
     Be concise in your responses. When using tools, explain what you're doing briefly.";

/// That diagrams are drawn, and how to keep a label from breaking one.
///
/// Phoenix's sentence, worded for Kingdom's surface: Phoenix says "Phoenix
/// renders", and here the reader is the chamber and the proposal card.
///
/// This one string has a history worth knowing before touching it. Kingdom
/// shipped the claim once while nothing rendered anything, and it cost a real
/// plan its whole turn -- asked to make proposals render markdown, the model
/// found the contradiction between the prompt and
/// `components/conversation.rs` and spent 25 of its 30 reasoning blocks
/// litigating it instead of proposing anything. It was deleted for that, with a
/// note saying "restore this the day a renderer exists".
///
/// `components/markdown.rs` is that renderer, so the sentence is back, and the
/// rule it illustrates still holds: what the prompt says about Kingdom must be
/// true of Kingdom. If the renderer is ever removed, this goes with it.
///
/// The parenthetical about quoting labels is Phoenix's too and is the half that
/// earns its keep -- an unquoted label containing brackets is the single most
/// common way a model's diagram fails to parse.
const MERMAID: &str = "The chamber renders Markdown mermaid code fences as diagrams -- \
     in the conversation and in a proposal's body alike; prefer them for diagrams when \
     useful. When a node label contains parentheses, quotes, or other punctuation, wrap \
     the label text in double quotes (e.g. `A[\"svc.Get(\\\"x\\\")\"]`) so Mermaid does not \
     read the punctuation as diagram syntax.";

/// That a round is the unit of cost, so independent calls belong together.
///
/// No Phoenix counterpart, kept for the same reason [`shared_machine_block`] is: it
/// states a fact about *Kingdom's* transport rather than improving on Phoenix's
/// wording. `copilot::armed` sets `parallel_tool_calls` and its comment already
/// says what that buys -- "(N-1) round trips whenever the model recognises a
/// batch as independent... every round resends the entire transcript, so a round
/// avoided is the whole conversation not re-sent." The capability was armed and
/// nothing had ever asked a model to use it.
///
/// Measured before it was written: across the four most recently approved plans,
/// 702 rounds produced 840 tool calls -- 1.20 per round, with 84% of rounds
/// carrying exactly one and not a single round in any of them carrying three.
/// Merging only the consecutive read-only rounds would have saved 6% of them,
/// and because the prefix is re-sent every round the byte saving is larger than
/// the round saving.
///
/// **Expect little of it.** `workspace_block`'s second clause is the cautionary
/// case: it fixed the `cd`-prefix habit outright for two plans and then lost to
/// a stronger prior the moment a plan worked across two repositories. A sentence
/// in a system prompt competes with everything else in the window. This one
/// costs three lines and may buy nothing.
///
/// The second clause is not padding. Told only to batch, a model batches a read
/// with the write that depends on it and then reasons from a result it never
/// saw; saying plainly when *not* to is what makes the instruction safe to
/// follow.
const BATCHING: &str = "If you intend to call several tools and there are no dependencies \
     between the calls, make all of the independent calls in the same reply rather than one \
     per reply. Every round re-sends the whole conversation, so four reads asked for together \
     cost a fraction of the same four asked for one at a time. When a call needs the result of \
     an earlier one, wait for it -- correctness first.";

/// That the machine has other tenants -- said differently depending on whether
/// this plan actually shares it.
///
/// Kept through the Phoenix port when its neighbours were dropped: Phoenix has
/// no equivalent because Phoenix does not have several agents sharing one
/// machine as its whole subject. This is Kingdom's own problem, and the one
/// place where having no Phoenix counterpart is a reason to keep something
/// rather than to delete it.
///
/// This block used to end with "pick an unusual free port explicitly rather than
/// taking a project's default". That was honest when Kingdom could only *watch*
/// two plans collide on 3000, and it is no longer true of the product:
/// [`crate::namespaces::net`] gives an isolated or sealed plan its own loopback,
/// its own port space and forwarding back to the host, precisely so that "no
/// agent has to be told to pick another port".
///
/// So for a walled-off plan the advice is now *false*, and a false sentence in a
/// prompt is followed as diligently as a true one -- it would put a dev server
/// on some invented port instead of the `:3000` that the forwarding, the
/// spyglass and the project's own config all expect.
///
/// For a plan on the machine's own network the port advice is gone too, but the
/// half that was never about port numbers stays: other tenants' processes are
/// not yours to kill, and what you start you stop. A model that finds 3000 taken
/// should say so -- which is what a real plan reasoned its way to unaided,
/// having collided with the King's own server on 3000.
fn shared_machine_block(isolation: kingdom_core::Isolation) -> String {
    if isolation.is_isolated() {
        "On ports and long-running processes. This plan has a network of its own: \
         its loopback is yours alone, so bind whatever port the project expects \
         -- :3000 here is not the user's :3000, and the ports you open are \
         forwarded back to him. Nothing you start can collide with another plan, \
         so do not invent an unusual port. Stop long-running processes when you \
         are done with them."
            .to_string()
    } else {
        "On ports and long-running processes. This machine is shared -- the \
         user's own Kingdom server and other plans may be running alongside \
         you. Never kill a process you did not start, and stop what you start. \
         If a port you need is already taken, say so rather than working \
         around it."
            .to_string()
    }
}

/// Phoenix's catalogue preamble, verbatim.
///
/// The last sentence is load-bearing: without it a model reads the paths in the
/// catalogue as an invitation to `cat` the file, which bypasses the argument
/// substitution and the base-directory line that make a skill usable.
const SKILLS_PREAMBLE: &str = "The following skills are available. Invoke them with the \
     `skill` tool (e.g. skill(skill_name=\"build\")). Do not cat SKILL.md files directly.\n";

/// Where the model is standing.
///
/// Kingdom's counterpart to the worktree grounding note Phoenix emits from
/// `repo_root_from_phoenix_worktree`. The model is not told this anywhere else:
/// `begin_plan` records the workspace as a [`kingdom_core::NoteKind::Workspace`]
/// note, and `Plan::turns` deliberately withholds notes from the model -- so
/// without this an agent in a worktree at `<city>/.kingdom/<uuid>` has no idea
/// it is not in the project's own checkout, and will describe its work as having
/// changed the user's files.
///
/// The second clause is the block finishing its own job. Naming the directory
/// says *where* the model is standing; it took a count of 365 real `bash` calls,
/// 64% of which opened with a `cd` to this very path, to notice that nothing
/// said commands **start** here. `tools/bash.rs` says it too, at the one moment
/// it is acted on; this says it once, up front, for every tool that takes a path.
fn workspace_block(
    workspace: &kingdom_core::Workspace,
    isolation: kingdom_core::Isolation,
    allowed: &[kingdom_core::services::MountSpec],
) -> String {
    let mut out = format!(
        "Working directory: {}\nEvery command runs here, and every relative path is \
         resolved from here.\n",
        workspace.path
    );
    match (&workspace.branch, workspace.is_isolated()) {
        (Some(branch), true) => out.push_str(&format!(
            "This is an isolated worktree on branch {branch}. It is yours: the user's own \
             checkout is elsewhere and is not affected by what you do here."
        )),
        _ => out.push_str(
            "This is the project directory itself, with no isolation. Anything you change \
             here changes the user's own checkout.",
        ),
    }
    // Said only where it is true. A plan working on Kingdom itself will start a
    // rehearsal server, and `crate::tools::child_environment` has already
    // pointed that child at the offline model and at its own profile. Without
    // this sentence the plan discovers a picker on a paid model and "fixes" it
    // by choosing one, which is the cost the mechanism exists to avoid.
    if crate::tools::runs_a_kingdom(std::path::Path::new(&workspace.path)) {
        out.push_str(
            "\n\nThis is a checkout of Kingdom itself. A server you start here already \
             runs against the offline `mock` model and keeps its records inside this \
             workspace, so rehearsing costs the user nothing and leaves nothing behind \
             -- you do not need to choose a model in the picker.",
        );
    }

    // Said only where it is true, and it is worth saying: a sealed plan's
    // commands run against a filesystem that is *not* the user's, and a model
    // that does not know this reads an absent `~/.ssh` or an unfamiliar `ps`
    // output as a broken machine and starts trying to repair it. Naming the
    // boundary once, up front, is much cheaper than the exploration it saves.
    if isolation.is_sealed() {
        out.push_str(
            "\n\nThis plan is sealed: it has its own network, its own filesystem and \
             its own process table. You can see this workspace, its project's git \
             directory, a read-only system, and whatever folders the user has \
             allowed in -- and nothing else of his. A file you cannot find outside \
             those is not missing, it is simply not shared with you; say so rather \
             than working around it. `ps` shows only this plan's own processes.",
        );

        // Where it is actually standing. The workspace is bound at `/app` as
        // well as at the path named above, and `/app` is where commands start
        // -- so `pwd` disagrees with the working directory this prompt opens
        // with, and a model that was not told reads that as having been moved
        // somewhere unexpected and starts investigating the machine. Both
        // paths are the same directory, which is the part worth saying.
        let _ = write!(
            out,
            "\n\nYour commands start at `{}`, which is this same workspace under a \
             shorter name: `{}` and {} are one directory, and a file written \
             through either is visible through the other. Both work; `{}` is \
             simply what `pwd` will tell you.",
            crate::namespaces::mount::WORKSPACE_AT,
            crate::namespaces::mount::WORKSPACE_AT,
            workspace.path,
            crate::namespaces::mount::WORKSPACE_AT,
        );

        // The folders themselves, named. Without this the model knows it is
        // fenced in but not where the fence is, which is the difference between
        // "I cannot see ~/.ssh, so I will say so" and a turn spent hunting for
        // a toolchain that was never shared.
        if !allowed.is_empty() {
            out.push_str("\n\nFolders shared with you, beyond the system:\n");
            for mount in allowed {
                let _ = write!(
                    out,
                    "\n- {} ({})",
                    mount.path,
                    if mount.mode.is_writable() {
                        "you may write here"
                    } else {
                        "read-only"
                    }
                );
            }
            out.push_str(
                "\n\nThe user adds to this list from the isolation panel when he opens \
                 a plan. If something you genuinely need is missing, say which folder \
                 and why rather than trying to work without it.",
            );
        }
    }
    out
}

/// The city's shared services, and where this particular plan reaches them.
///
/// Empty for almost every project, which is what keeps this free: a city with
/// no manifest adds nothing to any prompt.
///
/// # Why the address depends on the plan
///
/// Every model's prior for "connect to the database" is `localhost`, and this
/// block used to exist to fight that prior: a container is not on the host's
/// loopback, and a plan with a network of its own reaches `127.0.0.1` inside
/// its *own* namespace where nothing was listening.
///
/// `namespaces::net::open_wells` makes something listen there. So for an isolated plan
/// the prior is now simply *correct*, and the block says so instead of warning
/// against it -- the shortest possible instruction, which is the one most
/// likely to be followed. For a plan on the machine's network nothing has
/// changed and the warning stands, because for that plan it is still true.
///
/// Telling an isolated plan the old warning would be worse than saying nothing:
/// a model that believes `localhost` will fail writes code to avoid it, and a
/// false sentence in a prompt is followed as diligently as a true one.
///
/// # And why no environment variable is mentioned
///
/// Because none is set. There is one way to reach a shared resource and this is
/// it. A prompt that offered `$MONGODB_URI` as an alternative would send a model
/// looking for a variable that does not exist, and reading an empty one is a
/// failure that reads as a broken database.
fn services_block(plan: &kingdom_core::PlanId, city_root: &Path) -> String {
    let running = crate::services::running_in(city_root);
    if running.is_empty() {
        return String::new();
    }

    let mut out = String::from(
        "This project has shared services running -- one set, shared by every \
         agent working on it, not one per agent. Reach them at these addresses:\n",
    );
    // The address as *this plan* reaches it, decided in one place for every
    // surface that says it. See `services::address_for`.
    let addresses: Vec<String> = running
        .iter()
        .map(|service| crate::services::address_for(plan, service))
        .collect();

    for (service, address) in running.iter().zip(&addresses) {
        // The scope is named because it changes what "shared" means. A city's
        // well is shared with the other agents on this project; the King's is
        // shared with every agent on every project he has open, and an agent
        // about to drop a collection should know which of those it is holding.
        let shared_with = match service.scope {
            kingdom_core::ServiceScope::Host => "shared across every project on this machine",
            kingdom_core::ServiceScope::City => "shared by this project",
        };
        let _ = writeln!(
            out,
            "\n- **{}** ({}) at `{address}` -- {shared_with}",
            service.name, service.what,
        );
    }

    let everything_local = addresses
        .iter()
        .all(|address| address.starts_with("localhost:"));

    if everything_local {
        out.push_str(
            "\nThese are on **your own loopback**: this plan has a network of its \
             own, and Kingdom has put each service on `localhost` at its usual \
             port inside it. So connect the ordinary way -- \
             `mongodb://localhost:27017`, `postgres://localhost:5432` -- and it \
             works. There is nothing to set up, no environment variable to read, \
             and no address to configure before running the project.\n\nYour \
             `localhost` is yours alone -- it is not the user's, and not another \
             agent's -- so nothing you bind here can collide with them.",
        );
    } else {
        out.push_str(
            "\nUse exactly the addresses above. Any that is not `localhost` is a \
             container this plan reaches directly, and connecting to `127.0.0.1` \
             for it will fail. No environment variable holds these; the addresses \
             here are the whole answer.",
        );
    }

    // Said in both cases, and it matters more in the first: an address that is
    // easy to reach is easy to reach carelessly, and the data behind it still
    // belongs to everyone.
    out.push_str(
        "\n\nThey are shared, so treat the data in them as someone else's too: \
         other agents are reading and writing it at the same time.",
    );
    out
}

/// What the model may do, in the words it is told it.
fn permissions_block(permissions: Permissions, approved: bool) -> String {
    match permissions {
        Permissions::ReadOnly => SUBAGENT.to_string(),
        Permissions::Propose => PROPOSE.to_string(),
        Permissions::Full if approved => format!("{FULL}\n\n{CARRYING_OUT}"),
        Permissions::Full => FULL.to_string(),
    }
}

/// Phoenix's `sub_agent_suffix`, adapted.
///
/// Phoenix's names `submit_result` and `submit_error`, which Kingdom's subagents
/// do not have -- a subagent here simply answers, and the reply *is* the report.
/// The rest of the framing is Phoenix's: one job, then stop.
const SUBAGENT: &str = "You are a sub-agent working on a specific task. You can read and \
     search; you cannot run commands, edit files, or spawn sub-agents of your own. When you \
     have your answer, reply with it: your reply is the report, and the conversation ends \
     there. Answer concretely, citing the files you looked at.";

/// Phoenix's `mode_explore`, adapted to Kingdom's proposal flow.
///
/// Phoenix's block describes drafting a task *file* under a tasks directory and
/// pointing `propose_task` at it, with `patch` allowlisted to that directory.
/// Kingdom now does the same thing with its own names: the draft is a single
/// file at [`crate::tools::propose_plan::DRAFT`], `patch` is scoped to it, and
/// `propose_plan` takes its path.
///
/// This block used to say a proposing plan held no `patch` at all, and the
/// two-step workflow was dropped as machinery Kingdom did not need. That was the
/// mistake: with nowhere to write the plan down, a real plan investigated for 21
/// rounds and never proposed. The full story is in `tools/propose_plan.rs`.
///
/// Note what is *not* here. No advice about re-reading, about when to stop
/// looking, or about how much investigation is enough -- Phoenix sends none, and
/// the drafting mechanism is what does that job. A paragraph asking the model to
/// conclude would be Kingdom re-inventing the guidance this port removed.
const PROPOSE: &str = "You are in Propose mode. This conversation is read-only for source \
     files -- you can read files, search, run commands, analyse and discuss the codebase, but \
     you cannot modify code.\n\n\
     `bash` is available and is not sandboxed: a command naming an absolute path can write \
     anywhere on this machine. That boundary is one you are trusted to keep, not one Kingdom \
     enforces. Use it to look -- `git log`, `cargo tree`, running the tests to see which fail \
     -- and never to change.\n\n\
     Workflow for proposing work:\n\
     1. Draft the plan as markdown at `.kingdom/draft.md`, using `patch` with operation \
        `overwrite`. Start it with an `# H1` title, then the plan itself. Write to it as \
        soon as you know roughly what you intend and revise it with further patches as you \
        learn more -- it is where the plan lives while you work it out, not a report you \
        write at the end. `patch` is restricted to that one file in this mode.\n\
     2. Call `propose_plan` with `draft` set to that path. Say what you would change, in \
        which files, and why; say what you checked and what you are assuming. The user will \
        review and can approve, request revisions, or reject. On approval you gain full \
        write access and carry the plan out in this same conversation.\n\n\
     If the user asks you to change code directly, explain that you must propose a plan first.";

/// Phoenix's `mode_work`, adapted.
///
/// Phoenix's names a branch, a base branch and a worktree path, and ends with
/// the taskmd status rename. Kingdom states the worktree in its own block above
/// and has no task files, so what carries over is the working instruction and
/// the handoff.
const FULL: &str = "You are in Work mode: you have full tool access and are working in the \
     directory above. Use the tools -- read before you change, and check your work by running \
     it rather than by assuming. When the work is complete, let the user know what you did and \
     what it means for them, concisely and without repeating output they can already see.";

const CARRYING_OUT: &str = "You are carrying out a plan the user approved. It is above, in \
     the record of your own `propose_plan` call. Follow it; if you find it was wrong, say so \
     rather than quietly doing something else.";

/// Every guidance file from `from` up to `stop`, root-most first.
///
/// Matches Phoenix's `discover_guidance_files`. Three things are load-bearing.
///
/// **The bound.** The walk stops after `stop` -- the kingdom root -- and never
/// climbs past it. Guidance above the kingdom belongs to the user's machine
/// rather than to this kingdom, and picking it up would let a file they had
/// forgotten about instruct every plan in every project: a stray `AGENTS.md` in
/// `$HOME`, or in any folder that happens to be a parent of several checkouts,
/// would silently join every system prompt Kingdom ever sends. That is both a
/// prompt-injection surface and a per-round token cost, and the content dedup
/// below does not mitigate it, because such a file is a *different* file rather
/// than a duplicate of one.
///
/// A workspace outside the kingdom entirely -- which nothing produces today,
/// but a future in-place plan on a path elsewhere would -- reads its own
/// directory and stops, rather than walking to `/` because the bound was never
/// met.
///
/// **The order.** Root guidance first, project guidance last, so the more
/// specific file is the one the model reads most recently.
///
/// **The dedup.** A worktree contains a checkout of the city's own tracked
/// `AGENTS.md`, so walking up from `<city>/.kingdom/<uuid>` finds the same file
/// twice -- once in the worktree and once in the city. Deduping on *content*
/// rather than path is what catches that, because the two copies have different
/// paths and identical bodies.
fn discover_guidance(from: &Path, stop: &Path) -> Vec<Guidance> {
    let mut found: Vec<Guidance> = Vec::new();
    let mut here = Some(from.to_path_buf());

    // A workspace that is not under the kingdom at all would never meet the
    // bound below, and would walk to `/` -- which is the failure this bound
    // exists to prevent, reached by the one path that skips it. Nothing
    // produces such a workspace today; this makes the guarantee hold anyway,
    // rather than resting on that staying true.
    let bounded = from.starts_with(stop);

    while let Some(dir) = here {
        if let Some(file) = read_guidance(&dir) {
            found.push(file);
        }
        // Inclusive of `stop` itself: a kingdom-wide `AGENTS.md` sitting in the
        // dev folder is guidance for every city in it, and is exactly the file
        // this walk should reach.
        if !bounded || dir == stop {
            break;
        }
        here = dir.parent().map(Path::to_path_buf);
    }

    // Gathered leaf-first; the model should read root-first.
    found.reverse();

    let mut seen: HashSet<u64> = HashSet::new();
    let mut budget = MOST_GUIDANCE;
    found.retain(|file| {
        if !seen.insert(hash(&file.body)) {
            return false;
        }
        match budget.checked_sub(file.body.len()) {
            Some(left) => {
                budget = left;
                true
            }
            None => false,
        }
    });

    found
}

/// The first guidance file in one directory, if any.
fn read_guidance(dir: &Path) -> Option<Guidance> {
    GUIDANCE_NAMES.iter().find_map(|name| {
        let path: PathBuf = dir.join(name);
        let body = std::fs::read_to_string(&path).ok()?;
        Some(Guidance {
            path: path.display().to_string(),
            body,
        })
    })
}

fn hash(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(name), body).unwrap();
    }

    fn temp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "kingdom-system-prompt-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// `isolation` is an argument rather than a default because the shared
    /// machine block now says different things on either side of it.
    fn prompt_with(
        permissions: Permissions,
        approved: bool,
        isolation: kingdom_core::Isolation,
    ) -> SystemPrompt {
        SystemPrompt {
            city: CityBrief::default(),
            workspace: String::new(),
            permissions: permissions_block(permissions, approved),
            guidance: Vec::new(),
            skills: Vec::new(),
            shared_machine: shared_machine_block(isolation),
            services: String::new(),
        }
    }

    /// The case the content-hash dedup exists for, and the reason it is a
    /// *content* hash rather than a path one.
    ///
    /// A plan works in `<city>/.kingdom/<uuid>`, which is a checkout of the
    /// city -- so the city's tracked `AGENTS.md` is physically present at two
    /// different paths on the way up. Sending it twice would waste tokens on
    /// every round of every turn and, worse, would read to the model as
    /// emphasis.
    #[test]
    fn a_worktrees_copy_of_its_citys_guidance_is_not_sent_twice() {
        let root = temp();
        let city = root.join("city");
        let worktree = city.join(".kingdom/abc123");

        write(&city, "AGENTS.md", "city rules");
        // The worktree is a checkout: the same file, byte for byte.
        write(&worktree, "AGENTS.md", "city rules");

        let found = discover_guidance(&worktree, &root);

        let ours: Vec<&Guidance> = found.iter().filter(|g| g.body == "city rules").collect();
        assert_eq!(
            ours.len(),
            1,
            "the city's guidance appears at two paths and must be sent once: {found:#?}"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// The more specific file reads last, so it wins any disagreement.
    #[test]
    fn specific_guidance_reads_after_general() {
        let root = temp();
        let city = root.join("city");

        write(&root, "AGENTS.md", "kingdom rules");
        write(&city, "AGENTS.md", "city rules");

        let found = discover_guidance(&city, &root);
        let bodies: Vec<&str> = found.iter().map(|g| g.body.as_str()).collect();

        let kingdom = bodies.iter().position(|b| *b == "kingdom rules");
        let city_at = bodies.iter().position(|b| *b == "city rules");
        assert!(kingdom.is_some() && city_at.is_some(), "{found:#?}");
        assert!(
            kingdom < city_at,
            "specific guidance must read last: {found:#?}"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// The walk stops at the kingdom root. Guidance above it belongs to the
    /// user's machine rather than to this kingdom, and picking it up would let
    /// a file they had forgotten about instruct every plan in every project.
    ///
    /// This is a prompt-injection surface as much as a token cost: a stray
    /// `AGENTS.md` in `$HOME`, or in any folder that happens to be a parent of
    /// several checkouts, would otherwise join every system prompt Kingdom
    /// sends. The content dedup does not catch it -- it is a different file,
    /// not a duplicate of one.
    #[test]
    fn guidance_above_the_kingdom_is_left_alone() {
        let outer = temp();
        let root = outer.join("kingdom");
        let city = root.join("city");

        write(&outer, "AGENTS.md", "somebody else's rules");
        write(&root, "AGENTS.md", "kingdom rules");
        write(&city, "AGENTS.md", "city rules");

        let found = discover_guidance(&city, &root);
        let bodies: Vec<&str> = found.iter().map(|g| g.body.as_str()).collect();

        assert!(
            !bodies.contains(&"somebody else's rules"),
            "guidance above the kingdom must never reach the model: {found:#?}"
        );
        // The kingdom's own file is *inside* the bound: it is guidance for
        // every city in the dev folder, and is exactly what the walk should
        // reach.
        assert_eq!(bodies, vec!["kingdom rules", "city rules"]);

        std::fs::remove_dir_all(&outer).ok();
    }

    /// A workspace that is not under the kingdom at all must not fall out of
    /// the bound and walk to `/`.
    ///
    /// Nothing produces such a workspace today, which is exactly why this is
    /// worth pinning: the guarantee above would otherwise rest on that staying
    /// true, and the one path that skips the bound is the one that re-opens the
    /// hole it exists to close.
    #[test]
    fn a_workspace_outside_the_kingdom_does_not_climb_out_of_it() {
        let outer = temp();
        let root = outer.join("kingdom");
        let elsewhere = outer.join("elsewhere");

        write(&outer, "AGENTS.md", "somebody else's rules");
        write(&root, "AGENTS.md", "kingdom rules");
        write(&elsewhere, "AGENTS.md", "its own rules");

        let found = discover_guidance(&elsewhere, &root);
        let bodies: Vec<&str> = found.iter().map(|g| g.body.as_str()).collect();

        assert_eq!(
            bodies,
            vec!["its own rules"],
            "an unbounded walk must read its own directory and stop: {found:#?}"
        );

        std::fs::remove_dir_all(&outer).ok();
    }

    /// The remit is the last thing in the prompt, under every permission level.
    ///
    /// This is the whole reason the order was taken from Phoenix. Kingdom used
    /// to render the remit early and then follow it with up to 64 KB of
    /// `AGENTS.md`, leaving the instruction that says what the model may do
    /// right now as the least recent thing it read. Anything appended after the
    /// remit puts that distance back, so this pins the ordering rather than the
    /// wording.
    #[test]
    fn the_remit_is_the_last_thing_the_model_reads() {
        for permissions in [
            Permissions::ReadOnly,
            Permissions::Propose,
            Permissions::Full,
        ] {
            let mut prompt = prompt_with(permissions, false, kingdom_core::Isolation::Shared);
            prompt.guidance = vec![Guidance {
                path: "/somewhere/AGENTS.md".to_string(),
                body: "project rules".to_string(),
            }];
            prompt.skills = vec![Skill {
                name: "build".to_string(),
                description: "builds it".to_string(),
                argument_hint: None,
                path: PathBuf::from("/somewhere/.claude/skills/build/SKILL.md"),
            }];
            prompt.workspace = workspace_block(
                &kingdom_core::Workspace::in_place("/somewhere"),
                kingdom_core::Isolation::Shared,
                &[],
            );

            let rendered = prompt.render();

            assert!(
                rendered.trim_end().ends_with(prompt.permissions.trim()),
                "{permissions:?}: the remit must be last, not followed by guidance or \
                 skills. Ends with: {:?}",
                &rendered[rendered.len().saturating_sub(200)..]
            );
        }
    }

    /// Phoenix sends no project summary and no file listing, and neither does
    /// Kingdom now. The brief is still *carried* -- the mock reads it -- so this
    /// pins that carrying it does not put it back in the prompt.
    #[test]
    fn the_project_file_listing_is_not_sent() {
        let mut prompt = prompt_with(Permissions::Full, false, kingdom_core::Isolation::Shared);
        prompt.city = CityBrief {
            name: "somewhere".to_string(),
            path: "/somewhere".to_string(),
            stack: "Rust".to_string(),
            file_count: 3,
            has_git: true,
            dirty_files: 0,
            notable_paths: vec!["src/secret_plans.rs".to_string()],
        };

        let rendered = prompt.render();

        assert!(
            !rendered.contains("src/secret_plans.rs"),
            "the file listing costs tokens on every round and `search` answers \
             better: {rendered}"
        );
        assert!(!rendered.contains("Some files in this project"));
    }

    /// The catalogue is a heading plus one line per skill, and is absent
    /// entirely when a project has none -- an empty `<available_skills>` block
    /// would be a claim that there are skills.
    #[test]
    fn skills_are_catalogued_only_when_there_are_some() {
        let bare = prompt_with(Permissions::Full, false, kingdom_core::Isolation::Shared);
        assert!(!bare.render().contains("<available_skills>"));

        let mut with_skill = prompt_with(Permissions::Full, false, kingdom_core::Isolation::Shared);
        with_skill.skills = vec![Skill {
            name: "build".to_string(),
            description: "Builds the thing.".to_string(),
            argument_hint: None,
            path: PathBuf::from("/p/.claude/skills/build/SKILL.md"),
        }];

        let rendered = with_skill.render();
        assert!(rendered.contains("<available_skills>"));
        assert!(rendered.contains("**build**"));
        assert!(rendered.contains("Builds the thing."));
        // The body is fetched on demand; only metadata belongs here.
        assert!(rendered.contains("Do not cat SKILL.md files directly"));
    }

    /// Kingdom may tell a model its output is rendered, because it now is.
    ///
    /// This assertion is the *second* state of a string with a history. Phoenix
    /// says its conversation renders mermaid fences as diagrams; Kingdom
    /// shipped that claim once while nothing rendered anything, and it cost a
    /// real plan its entire turn -- asked to make proposals render markdown,
    /// the model found the contradiction and spent 25 of its 30 reasoning
    /// blocks arguing with the prompt instead of proposing anything. The test
    /// here used to forbid the word for that reason.
    ///
    /// `components/markdown.rs` now renders both the chamber's messages and a
    /// proposal's body, so the promise is kept and the hint is back. The rule
    /// it guards did not change, only which way it points: if the renderer ever
    /// goes, this test and the sentence it pins go with it.
    #[test]
    fn the_court_is_told_its_diagrams_are_drawn() {
        let rendered =
            prompt_with(Permissions::Full, false, kingdom_core::Isolation::Shared).render();

        assert!(
            rendered.to_lowercase().contains("mermaid"),
            "the chamber renders markdown and diagrams (components/markdown.rs); \
             a model that is not told will not draw one: {rendered}"
        );
        // The half that prevents actually broken diagrams.
        assert!(rendered.contains("wrap the label text in double quotes"));
    }

    /// The court is told the machine has other tenants on it.
    ///
    /// Phoenix has no equivalent, so the port's default -- delete what Phoenix
    /// lacks -- would have dropped it. It is kept because Kingdom's whole
    /// subject is several agents on one machine, and this is observed rather
    /// than hypothetical: a plan took the King's own port 3000.
    ///
    /// What is *not* said any more is the port advice. A plan on the host
    /// network must still leave other people's processes alone; it must not be
    /// told to invent a port number, because a taken port is now news for the
    /// King rather than a puzzle for the model.
    #[test]
    fn the_court_is_warned_about_the_shared_machine() {
        let rendered =
            prompt_with(Permissions::Full, false, kingdom_core::Isolation::Shared).render();
        assert!(rendered.contains("Never kill a process you did not start"));
        assert!(
            !rendered.contains("unusual"),
            "nothing should ask for an unusual port: {rendered}"
        );
    }

    /// A walled-off plan is told its ports are its own, not warned off them.
    ///
    /// `namespaces::net` gives an isolated or sealed plan its own loopback so
    /// that no agent has to be told to pick another port. Telling one to avoid
    /// a project's default would be a false sentence, and would put its dev
    /// server somewhere the forwarding and the spyglass do not expect.
    #[test]
    fn a_walled_off_plan_is_told_its_ports_are_its_own() {
        for isolation in [
            kingdom_core::Isolation::Isolated,
            kingdom_core::Isolation::Sealed,
        ] {
            let rendered = prompt_with(Permissions::Full, false, isolation).render();
            assert!(
                rendered.contains("bind whatever port the project expects"),
                "{isolation:?} has a loopback of its own and should be told so: {rendered}"
            );
            assert!(
                !rendered.contains("Never kill a process you did not start"),
                "{isolation:?} shares no process table with the King"
            );
        }
    }

    /// The court is asked to batch calls that do not depend on each other.
    ///
    /// The [`shared_machine_block`] case rather than the `label`/`since` case: no
    /// Phoenix counterpart, kept because it states a fact about Kingdom's own
    /// transport. `copilot::armed` sets `parallel_tool_calls` and nothing had
    /// ever asked a model to use it -- four real plans averaged 1.20 tool calls
    /// per round, with 84% of rounds carrying exactly one.
    ///
    /// Both halves are pinned. The instruction alone would have a model batch a
    /// read with the write that depends on it, which trades a token bill for a
    /// correctness bug, so the caveat is as load-bearing as the ask.
    ///
    /// It must also stay *before* the remit --
    /// [`the_remit_is_the_last_thing_the_model_reads`] enforces the other side
    /// of that, and this is the block most likely to be appended in the wrong
    /// place because it reads like a closing instruction.
    #[test]
    fn the_court_is_asked_to_batch_independent_calls() {
        let rendered =
            prompt_with(Permissions::Full, false, kingdom_core::Isolation::Shared).render();

        assert!(
            rendered.contains("make all of the independent calls in the same reply"),
            "`parallel_tool_calls` is armed and only the prompt can ask for it: {rendered}"
        );
        assert!(
            rendered.contains("When a call needs the result of an earlier one, wait for it"),
            "batching without the caveat buys tokens at the price of correctness"
        );

        let batching = rendered.find("independent calls").expect("just asserted");
        let remit = rendered
            .find("You are in Work mode")
            .expect("the remit renders for Full");
        assert!(
            batching < remit,
            "the remit is last; this must not be appended after it"
        );
    }

    /// The workspace block says commands *start* here, not merely where here is.
    ///
    /// Both kinds of workspace, because the sentence is about how tools resolve
    /// paths rather than about isolation -- an in-place plan needs it exactly as
    /// much as a worktree, and the two take different arms of the match below.
    ///
    /// This is the prompt's half of the same fix `tools/bash.rs` carries in its
    /// description. Stated twice on purpose: the tool says it at the moment a
    /// command is written, and this says it once for every tool that takes a
    /// path. 64% of real `bash` calls opened with a redundant `cd` while only
    /// the *location* was stated and never the fact that work begins there.
    #[test]
    fn the_workspace_block_says_commands_start_there() {
        let isolated = kingdom_core::Workspace {
            branch: Some("kingdom/a-plan".to_string()),
            id: Some("abc".to_string()),
            ..kingdom_core::Workspace::in_place("/dev/city/.kingdom/abc")
        };
        assert!(
            isolated.is_isolated(),
            "the worktree arm must be the one taken"
        );

        for workspace in [isolated, kingdom_core::Workspace::in_place("/dev/city")] {
            let block = workspace_block(&workspace, kingdom_core::Isolation::Shared, &[]);

            assert!(
                block.contains(&workspace.path),
                "the model must still be told where it stands: {block}"
            );
            assert!(
                block.contains("Every command runs here"),
                "naming the directory is not the same as saying work begins in \
                 it -- that gap is what buys a `cd` on every command: {block}"
            );
        }
    }

    /// A sealed plan is told its filesystem is not the user's.
    ///
    /// Without this the model reads an absent `~/.ssh`, an unfamiliar `ps` or a
    /// missing tool as a machine in need of repair, and spends a turn trying to
    /// fix it. The boundary is cheap to state and expensive to discover.
    #[test]
    fn a_sealed_plan_is_told_what_it_can_see() {
        let workspace = kingdom_core::Workspace::in_place("/dev/city");

        let sealed = workspace_block(
            &workspace,
            kingdom_core::Isolation::Sealed,
            &[kingdom_core::services::MountSpec {
                path: "~/.cargo".to_string(),
                mode: kingdom_core::services::MountMode::Rw,
            }],
        );
        assert!(
            sealed.contains("sealed"),
            "a sealed plan must be told so: {sealed}"
        );
        assert!(
            sealed.contains("~/.cargo") && sealed.contains("you may write here"),
            "a sealed plan must be told which folders it has, not merely that \
             it is fenced in: {sealed}"
        );
        assert!(
            sealed.contains("not missing"),
            "and told how to read an absent file, which is the actual failure \
             this prevents: {sealed}"
        );

        // And the other two modes say nothing of the kind, because for them it
        // would be false: their filesystem *is* the user's.
        for ordinary in [
            kingdom_core::Isolation::Shared,
            kingdom_core::Isolation::Isolated,
        ] {
            let block = workspace_block(&workspace, ordinary, &[]);
            assert!(
                !block.contains("sealed"),
                "{ordinary:?} shares the user's filesystem and must not claim \
                 otherwise: {block}"
            );
        }
    }

    /// An approved plan is told it is carrying out a plan; an unapproved one is
    /// not. Getting this backwards would have a fresh plan hunting the
    /// conversation for a proposal that was never made.
    #[test]
    fn only_an_approved_plan_is_told_to_follow_one() {
        assert!(permissions_block(Permissions::Full, true).contains("plan the user approved"));
        assert!(!permissions_block(Permissions::Full, false).contains("plan the user approved"));
    }

    /// An isolated plan is told the ordinary address works, rather than warned
    /// away from it.
    ///
    /// The reason this is a test and not a comment: the block used to say the
    /// exact opposite, and it said it *correctly* -- until a relay was put on
    /// the plan's loopback. A prompt that kept the old warning would be a false
    /// sentence, and a model follows a false instruction as diligently as a
    /// true one: it would write code to work around a problem that no longer
    /// exists.
    #[test]
    fn a_plan_with_the_well_on_its_loopback_is_told_to_use_localhost() {
        let city = temp().join("agora");
        crate::services::pretend_a_well_is_running(&city, 27017);
        let plan = kingdom_core::PlanId::new("plan-told-localhost");
        crate::namespaces::net::pretend_wells_are_open(&plan, &["172.31.4.10:27017"]);

        let block = services_block(&plan, &city);

        assert!(
            block.contains("localhost:27017"),
            "the address the agent should type must be the address it is shown: {block}"
        );
        assert!(
            !block.contains("172.31.4.10"),
            "showing both addresses invites the agent to pick the awkward one: {block}"
        );
        assert!(
            !block.contains("will fail"),
            "the warning against localhost is now false here: {block}"
        );
        // The point of the whole change, said plainly enough that a model
        // reaching for a config step does not take one.
        assert!(
            block.contains("nothing to set up"),
            "an agent that thinks it must configure an address will configure \
             one: {block}"
        );
        // No variable is set any more, so the prompt must not send a model
        // looking for one. Reading an empty `$MONGODB_URI` and connecting to
        // nothing reads as a broken database.
        assert!(
            !block.contains("MONGODB_URI") && !block.contains('$'),
            "nothing sets a variable now, so the prompt must not mention one: {block}"
        );
        // Easier to reach is easier to clobber, so this must survive.
        assert!(
            block.contains("someone else's"),
            "a shared database is still shared: {block}"
        );

        crate::namespaces::net::forget_namespace(&plan);
    }

    /// A plan on the machine's network is still warned, because for it the
    /// warning is still true.
    ///
    /// Such a plan has no loopback of its own, so nothing was relayed onto it
    /// and `127.0.0.1` really does reach nothing. Telling it otherwise would
    /// send it hunting for a fault in the project's own code.
    #[test]
    fn a_plan_on_the_machines_network_is_still_warned_off_localhost() {
        let city = temp().join("agora-shared");
        crate::services::pretend_a_well_is_running(&city, 27017);
        let plan = kingdom_core::PlanId::new("plan-on-the-machines-network");
        crate::namespaces::net::forget_namespace(&plan);

        let block = services_block(&plan, &city);

        assert!(
            block.contains("172.31.4.10:27017"),
            "the container's address is the only one that works here: {block}"
        );
        assert!(
            block.contains("will fail"),
            "this plan's 127.0.0.1 is the user's own, and reaches nothing: {block}"
        );
    }

    /// Two services on one port, one relayed: the prompt says a different
    /// address for each.
    ///
    /// The prompt and the plan's own environment must agree, and this is the
    /// case where agreeing is hard -- see
    /// `services::a_second_service_on_the_same_port_is_not_told_it_is_local`.
    /// Both go through `services::address_for` for exactly that reason, and
    /// this pins that the prompt really does.
    #[test]
    fn only_the_relayed_one_of_two_services_on_a_port_is_called_local() {
        let city = temp().join("agora-crowded");
        crate::services::pretend_a_named_well_is_running(&city, "cache", "172.31.4.10", 6379);
        crate::services::pretend_a_named_well_is_running(&city, "other", "172.31.9.10", 6379);
        let plan = kingdom_core::PlanId::new("plan-told-about-two-caches");
        crate::namespaces::net::pretend_wells_are_open(&plan, &["172.31.4.10:6379"]);

        let block = services_block(&plan, &city);

        assert!(
            block.contains("localhost:6379"),
            "the relayed service is on the loopback: {block}"
        );
        assert!(
            block.contains("172.31.9.10:6379"),
            "the other service is a different database and must be named as one: \
             {block}"
        );

        crate::namespaces::net::forget_namespace(&plan);
    }

    /// The block is built from the **project's** root, not the plan's
    /// workspace.
    ///
    /// The bug this pins, found by reading a real plan's prompt in a browser
    /// rather than by any test: `assemble` passed `CityBrief::path`, which is
    /// the plan's *worktree* (`<city>/.kingdom/<uuid>/` for an isolated plan).
    /// A shared resource is filed under the city's key, so the lookup matched
    /// nothing and the block came out **empty** -- a plan whose project shares
    /// a database was never told it had one.
    ///
    /// It was survivable while Kingdom also set `$MONGODB_URI`. Now that the
    /// prompt is the only channel, an empty block means an agent that cannot
    /// find its database at all.
    #[test]
    fn the_block_follows_the_project_not_the_plans_worktree() {
        let city = temp().join("agora-with-a-worktree");
        crate::services::pretend_a_well_is_running(&city, 27017);
        let plan = kingdom_core::PlanId::new("plan-in-a-worktree");
        crate::namespaces::net::forget_namespace(&plan);

        // Where an isolated plan actually works: inside the city, under
        // `.kingdom/`. Asking about *this* path must not be mistaken for the
        // project itself.
        let worktree = city.join(".kingdom").join("a-plan-id");
        std::fs::create_dir_all(&worktree).unwrap();

        assert!(
            services_block(&plan, &worktree).is_empty(),
            "a worktree is not a city, and must not be mistaken for one"
        );
        assert!(
            services_block(&plan, &city).contains("27017"),
            "the project's own root is what carries its shared resources"
        );
    }

    /// A project with no shared services adds nothing to any prompt.
    ///
    /// What keeps this block free for almost every city, and worth pinning
    /// while the block is being changed: an empty city that started emitting a
    /// heading would put a paragraph about databases in front of every agent
    /// on every round of every turn.
    #[test]
    fn a_city_with_no_wells_says_nothing_at_all() {
        let city = temp().join("no-wells");
        std::fs::create_dir_all(&city).unwrap();
        let plan = kingdom_core::PlanId::new("plan-in-an-ordinary-project");
        crate::namespaces::net::forget_namespace(&plan);

        assert!(services_block(&plan, &city).is_empty());
    }
}
