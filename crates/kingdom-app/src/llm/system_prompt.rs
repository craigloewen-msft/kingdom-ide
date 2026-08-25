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
//! **Phoenix wins on wording, never on facts about Kingdom.** [`SHARED_MACHINE`]
//! has no Phoenix counterpart and is kept anyway, because sharing one machine
//! between agents is Kingdom's own subject. [`MERMAID`] is the other way round:
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
    pub fn assemble(
        city: &CityBrief,
        workspace: &kingdom_core::Workspace,
        permissions: Permissions,
        approved: bool,
        kingdom_root: &Path,
    ) -> Self {
        let root = Path::new(&workspace.path);
        Self {
            city: city.clone(),
            workspace: workspace_block(workspace),
            permissions: permissions_block(permissions, approved),
            guidance: discover_guidance(root, kingdom_root),
            skills: crate::skills::discover(root),
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
        out.push_str(SHARED_MACHINE);

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

/// That the machine has other tenants, the King's own server among them.
///
/// Kingdom arbitrates no resources yet (AGENTS.md §4), so nothing detects two
/// plans binding one port -- or a plan binding the port the user is reading the
/// chamber on. That last one is observed, not hypothetical: a plan ran `cargo
/// leptos serve` with no override and collided with the King's own server on
/// 3000. It recovered unaided, having reasoned that the occupant was probably
/// the user's and should not be killed, which is the right instinct and one
/// nothing had told it to have.
///
/// Saying it costs a sentence and is not a substitute for arbitration. It only
/// makes the good outcome the likely one instead of the lucky one.
///
/// Kept through the Phoenix port when its neighbours were dropped: Phoenix has
/// no equivalent because Phoenix does not have several agents sharing one
/// machine as its whole subject. This is Kingdom's own problem, and the one
/// place where having no Phoenix counterpart is a reason to keep something
/// rather than to delete it.
const SHARED_MACHINE: &str = "On ports and long-running processes. This machine is shared -- \
     the user's own Kingdom server is very likely on port 3000, and other plans may be \
     working alongside you. Never kill a process you did not start. If you need to run a \
     server, pick an unusual free port explicitly rather than taking a project's default, \
     and stop it when you are done with it.";

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
fn workspace_block(workspace: &kingdom_core::Workspace) -> String {
    let mut out = format!("Working directory: {}\n", workspace.path);
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
/// Kingdom has none of that -- `propose_plan` takes a title and a body inline,
/// and a proposing plan holds no `patch` at all -- so that paragraph is dropped
/// rather than mapped onto something it does not describe.
///
/// What is kept is Phoenix's shape: what you may do, what you may not, how to
/// put work to the user, and what happens on approval.
const PROPOSE: &str = "You are in Propose mode. This conversation is read-only for source \
     files -- you can read files, search, run commands, analyse and discuss the codebase, but \
     you cannot modify code.\n\n\
     `bash` is available and is not sandboxed: a command naming an absolute path can write \
     anywhere on this machine. That boundary is one you are trusted to keep, not one Kingdom \
     enforces. Use it to look -- `git log`, `cargo tree`, running the tests to see which fail \
     -- and never to change.\n\n\
     Workflow for proposing work: call `propose_plan` with a title and the plan itself. Say \
     what you would change, in which files, and why; say what you checked and what you are \
     assuming. The user will review and can approve, request revisions, or reject. On \
     approval you gain full write access and carry the plan out in this same conversation.\n\n\
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

    fn prompt_with(permissions: Permissions, approved: bool) -> SystemPrompt {
        SystemPrompt {
            city: CityBrief::default(),
            workspace: String::new(),
            permissions: permissions_block(permissions, approved),
            guidance: Vec::new(),
            skills: Vec::new(),
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
        assert!(kingdom < city_at, "specific guidance must read last: {found:#?}");

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
        for permissions in [Permissions::ReadOnly, Permissions::Propose, Permissions::Full] {
            let mut prompt = prompt_with(permissions, false);
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
            prompt.workspace = workspace_block(&kingdom_core::Workspace::in_place("/somewhere"));

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
        let mut prompt = prompt_with(Permissions::Full, false);
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
        let bare = prompt_with(Permissions::Full, false);
        assert!(!bare.render().contains("<available_skills>"));

        let mut with_skill = prompt_with(Permissions::Full, false);
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
        let rendered = prompt_with(Permissions::Full, false).render();

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
    #[test]
    fn the_court_is_warned_about_the_shared_machine() {
        let rendered = prompt_with(Permissions::Full, false).render();
        assert!(rendered.contains("Never kill a process you did not start"));
        assert!(rendered.contains("3000"));
    }

    /// An approved plan is told it is carrying out a plan; an unapproved one is
    /// not. Getting this backwards would have a fresh plan hunting the
    /// conversation for a proposal that was never made.
    #[test]
    fn only_an_approved_plan_is_told_to_follow_one() {
        assert!(permissions_block(Permissions::Full, true).contains("plan the user approved"));
        assert!(!permissions_block(Permissions::Full, false).contains("plan the user approved"));
    }
}

