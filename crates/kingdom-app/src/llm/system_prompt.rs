//! The system prompt: everything the model is told before it is asked anything.
//!
//! Server-only, and lifted out of the provider on purpose. This used to be
//! built inside `copilot.rs`, which made it *Copilot's* prompt: a second
//! provider would have had to reinvent it, and the two would have drifted the
//! first time either was touched. What a model is told about the work is
//! content, not transport, so it is assembled once here and every provider
//! renders the same words.
//!
//! It tells the model where it is standing, what it may touch, and what the
//! project expects of it.

use super::CityBrief;
use kingdom_core::Permissions;
use std::collections::HashSet;
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
    /// The project. Kept as the brief rather than as rendered prose because a
    /// provider occasionally needs a *fact* from it -- the city's name for a
    /// fallback headline -- and re-parsing it out of the rendering would be
    /// absurd.
    pub city: CityBrief,
    /// Where the model is standing, and whether it is isolated.
    pub workspace: String,
    /// What it may do, and what it must not.
    pub permissions: String,
    /// Every `AGENTS.md` found on the way up from the workspace.
    pub guidance: Vec<Guidance>,
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
    /// `root` bounds the walk: guidance is gathered from the workspace up to
    /// the kingdom root and no further, so a stray `AGENTS.md` in the user's
    /// home directory cannot silently instruct every plan in every project.
    pub fn assemble(
        city: &CityBrief,
        workspace: &kingdom_core::Workspace,
        permissions: Permissions,
        approved: bool,
        root: &Path,
    ) -> Self {
        Self {
            city: city.clone(),
            workspace: workspace_block(workspace),
            permissions: permissions_block(permissions, approved),
            guidance: discover_guidance(Path::new(&workspace.path), root),
        }
    }

    /// The prompt as one system prompt.
    ///
    /// Order carries reasoning and is not arbitrary. The remit comes first,
    /// then the standing advice -- how to look things up, and how to think
    /// about tests -- and project guidance comes last: a project's own rules
    /// are the most specific thing here, so they arrive last and win any
    /// disagreement with the generic advice above them.
    pub fn render(&self) -> String {
        let mut out = String::from(PREAMBLE);

        out.push_str("\n\n");
        out.push_str(&self.city.render());

        if !self.workspace.is_empty() {
            out.push('\n');
            out.push_str(&self.workspace);
        }

        out.push('\n');
        out.push_str(&self.permissions);

        out.push_str("\n\n");
        out.push_str(ENDING_A_TURN);

        out.push_str("\n\n");
        out.push_str(ECONOMY);

        out.push_str("\n\n");
        out.push_str(TESTING);

        if !self.guidance.is_empty() {
            out.push_str("\n\n<project_guidance>\n");
            for (i, file) in self.guidance.iter().enumerate() {
                if i > 0 {
                    out.push_str("\n---\n\n");
                }
                out.push_str(&format!("<!-- From: {} -->\n", file.path));
                out.push_str(&file.body);
                if !file.body.ends_with('\n') {
                    out.push('\n');
                }
            }
            out.push_str("</project_guidance>");
        }

        out.push_str("\n\n");
        out.push_str(SHARED_MACHINE);

        out.push_str("\n\n");
        out.push_str(SCREENSHOTS);

        out
    }
}

const PREAMBLE: &str = "You are a senior software engineer helping with one project.";

/// What ending a turn means, told plainly because the model cannot infer it.
///
/// Kingdom ends the turn the moment a reply arrives carrying prose and no tool
/// call: [`crate::llm::Reply::Spoke`] settles the plan and hands control back to
/// the user. Models are trained to *narrate* before acting -- "I'll start by
/// reading the router" -- and in most harnesses that preamble is followed by
/// tool calls in the same reply. Here it silently finishes the job.
///
/// Three real plans died on their opening sentence this way, each parked in
/// front of the user having done nothing, and the only way on was for them to
/// type "Keep going". The model was behaving reasonably; nothing had told it
/// what prose costs. So this says it.
const ENDING_A_TURN: &str = "How a turn ends. Replying with prose and no tool call ends your turn \
     and hands control back to the user -- so narration is not free here. If you mean to keep \
     working, put the tool call in the same reply as the words; do not announce what you are \
     about to do and stop. Speak only when you have something for them: an answer, a question \
     you cannot proceed without, or a report that the work is done.";

/// Counters the model's "more tests is better" prior with a cost model.
///
/// Placed before `<project_guidance>` so a project with stricter rules of its
/// own overrides this rather than arguing with it.
const TESTING: &str = "Tests are a liability as well as an asset: every test costs review \
     time, runtime, and future maintenance. Add the minimal set that earns its place -- a \
     test for behaviour a caller depends on, a regression test pinning a bug you just fixed, \
     or a non-obvious edge case. Do not add tests that restate the implementation, assert \
     trivial accessors, or duplicate coverage that already exists. If a change needs no new \
     test, say so instead of inventing one.";

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
const SHARED_MACHINE: &str = "On ports and long-running processes. This machine is shared -- \
     the user's own Kingdom server is very likely on port 3000, and other plans may be \
     working alongside you. Never kill a process you did not start. If you need to run a \
     server, pick an unusual free port explicitly rather than taking a project's default, \
     and stop it when you are done with it.";

/// That a screenshot is *seen*, not merely saved.
///
/// Without this a model reasonably narrates "I've saved a screenshot you can
/// open at /home/...", which was true when only `read_image` could look at one
/// and is now simply false -- the chamber renders it under the deed that took
/// it (`components/conversation.rs`, served by `artifact.rs`). Left unsaid, the
/// model also keeps calling `read_image` purely to describe a page back to a
/// user who is already looking at it, which spends a turn and a slice of
/// context on prose nobody needs.
const SCREENSHOTS: &str = "On screenshots. browser_take_screenshot is shown to the user in the \
     conversation, under the call that took it -- so it is evidence you can point at rather than \
     a file they have to go and open. Do not tell them where it was saved. Call read_image on it \
     only when *you* need to see the page to decide what to do next; if the picture is for them, \
     taking it is enough.";

/// Where the model is standing.
///
/// The model is not told this anywhere else. `begin_plan` records the workspace
/// as a [`kingdom_core::NoteKind::Workspace`] note, and `Plan::turns`
/// deliberately withholds notes from the model -- so without this block an
/// agent working in a worktree at `<city>/.kingdom/<uuid>` has no idea it is
/// not in the project's own checkout, and will happily describe its work as
/// having changed the user's files.
fn workspace_block(workspace: &kingdom_core::Workspace) -> String {
    let mut out = format!("Working directory: {}\n", workspace.path);
    match (&workspace.branch, workspace.is_isolated()) {
        (Some(branch), true) => out.push_str(&format!(
            "This is an isolated worktree on branch {branch}. It is yours: the user's own \
             checkout is elsewhere and is not affected by what you do here.\n"
        )),
        _ => out.push_str(
            "This is the project directory itself, with no isolation. Anything you change \
             here changes the user's own checkout.\n",
        ),
    }
    out
}

/// What the model may do, in the words it is told it.
fn permissions_block(permissions: Permissions, approved: bool) -> String {
    match permissions {
        Permissions::ReadOnly => READ_ONLY.to_string(),
        Permissions::Propose => PROPOSE.to_string(),
        Permissions::Full if approved => format!("{FULL}\n\n{CARRYING_OUT}"),
        Permissions::Full => FULL.to_string(),
    }
}

const READ_ONLY: &str = "\nYou were sent to answer one question and report back. You can read \
     and search, and that is all: you cannot run commands, edit files, or spawn subagents of \
     your own. Answer what you were asked, concretely, citing the files you looked at.";

/// The proposing block: the heart of this whole arrangement.
///
/// Note what it says about `bash`, and why it says it. The tool list is not a
/// sandbox -- `Sandbox::root` is explicit that the path boundary does not
/// contain a shell -- so a command that names an absolute path can write
/// anywhere. Withholding `bash` would buy a guarantee Kingdom cannot keep while
/// costing the model `git log`, `cargo tree`, and running the failing test it
/// is proposing to fix. So the limit is stated as what it is: a boundary the
/// model is trusted to keep. Pretending otherwise would be worse, because the
/// user would believe in a fence that is not there.
const PROPOSE: &str = "\nYou are drawing up a plan, not carrying it out. Read, search, and \
     run whatever you need in order to understand the work -- but change nothing. No edits \
     to files, no commits, nothing written into the project.\n\n\
     You have `bash`, and it is not fenced in: a command that names an absolute path can \
     write anywhere on this machine. That boundary is one you are trusted to keep, not one \
     Kingdom enforces. Use it to look -- `git log`, `cargo tree`, running the tests to see \
     which fail -- and never to change.\n\n\
     When you know what should be done, call `propose_plan` with a title and the plan \
     itself. Say what you would change, in which files, and why; say what you checked and \
     what you are assuming. The user reads it and either starts you on it or sends back \
     changes, and you cannot edit anything until they do.\n\n\
     If they ask you to change something directly, explain that you must put a plan to them \
     first.";

/// Counters the model's instinct to keep looking.
///
/// Every tool result is resent on every round, so an investigation's cost grows
/// with the square of its length -- and a model that has lost the thread of its
/// own plan re-reads what it has already read rather than concluding. The
/// observed failure was 24 rounds of reading with no proposal at the end of it.
///
/// Applies to any remit that can look around, which is every one of them.
const ECONOMY: &str = "On looking things up. Everything you read stays in front of you for the \
     rest of the conversation, so reading is not free and re-reading is pure cost. Prefer \
     `search` to find where something lives, then `read_file` to read just that part -- a \
     whole large file is rarely what you need. Before opening something, check whether it is \
     already above: if you have read it, you still have it.\n\n\
     Stop when you know enough to be useful, not when you have run out of things to look at. \
     A plan that names what it did not check is worth more than one that checked everything \
     and arrived too late. State your assumptions instead of eliminating them.";

const FULL: &str = "\nYou have tools and are working in the directory above. Use them: read \
     before you change, and check your work by running it rather than by assuming. The file \
     list is a starting point, not the whole project. When you have finished, reply with \
     what you did and what it means for the reader -- concisely, and without repeating the \
     output of commands they can already see.";

const CARRYING_OUT: &str = "You are carrying out a plan the user approved. It is above, in \
     the record of your own `propose_plan` call. Follow it; if you find it was wrong, say so \
     rather than quietly doing something else.";

/// Every guidance file from `from` up to and including `root`, root-most first.
///
/// Two things are load-bearing.
///
/// **The order.** Root guidance first, project guidance last, so the more
/// specific file is the one the model reads most recently.
///
/// **The dedup.** A worktree contains a checkout of the city's own tracked
/// `AGENTS.md`, so walking up from `<city>/.kingdom/<uuid>` finds the same file
/// twice -- once in the worktree and once in the city. Deduping on *content*
/// rather than path is what catches that, because the two copies have different
/// paths and identical bodies.
fn discover_guidance(from: &Path, root: &Path) -> Vec<Guidance> {
    let mut found: Vec<Guidance> = Vec::new();
    let mut here = Some(from.to_path_buf());
    let stop = root.parent().map(Path::to_path_buf);

    while let Some(dir) = here {
        if let Some(file) = read_guidance(&dir) {
            found.push(file);
        }
        // The kingdom root is included, then the walk stops: guidance above it
        // belongs to the user's machine, not to this kingdom.
        if dir == root {
            break;
        }
        let up = dir.parent().map(Path::to_path_buf);
        if up == stop {
            break;
        }
        here = up;
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

    /// The case the content-hash dedup exists for, and the reason it is a
    /// *content* hash rather than a path one.
    ///
    /// A plan works in `<city>/.kingdom/<uuid>`, which is a checkout of the
    /// city -- so the city's tracked `AGENTS.md` is physically present at two
    /// different paths on the way up. Sending it twice would waste tokens on
    /// every round of every turn and, worse, would read to the model as
    /// emphasis.
    ///
    /// The ordering half matters for the same reason the walk is bounded: the
    /// kingdom's rules are general, the city's are specific, and the specific
    /// one has to arrive last to win.
    #[test]
    fn a_worktrees_copy_of_its_citys_guidance_is_not_sent_twice() {
        let root = temp();
        let city = root.join("city");
        let worktree = city.join(".kingdom/abc123");

        write(&root, "AGENTS.md", "kingdom rules");
        write(&city, "AGENTS.md", "city rules");
        // The worktree is a checkout: the same file, byte for byte.
        write(&worktree, "AGENTS.md", "city rules");

        let found = discover_guidance(&worktree, &root);

        assert_eq!(
            found.len(),
            2,
            "the city's guidance appears at two paths and must be sent once: {found:#?}"
        );
        assert_eq!(found[0].body, "kingdom rules", "root guidance reads first");
        assert_eq!(found[1].body, "city rules", "the specific file reads last");

        std::fs::remove_dir_all(&root).ok();
    }

    /// Every remit that can act must be told that prose ends its turn.
    ///
    /// This is the whole fix for the failure that killed three real plans on
    /// their opening sentence: the model wrote "I'll start by reading the
    /// router", Kingdom read a tool-call-less reply as the finished answer and
    /// parked the plan in front of the user having done nothing. The model was
    /// not being lazy -- nothing had told it what prose costs here.
    ///
    /// Pinned across remits because the trap is not specific to one: a proposing
    /// plan and a working plan both end their turn the same way.
    #[test]
    fn every_acting_remit_is_told_that_prose_ends_the_turn() {
        for permissions in [Permissions::Propose, Permissions::Full] {
            let prompt = SystemPrompt {
                city: CityBrief::default(),
                workspace: String::new(),
                permissions: permissions_block(permissions, false),
                guidance: Vec::new(),
            };

            assert!(
                prompt.render().contains(ENDING_A_TURN),
                "{permissions:?} must be told that a reply without a tool call hands \
                 the plan back to the user"
            );
        }
    }

    /// The walk stops at the kingdom root. Guidance above it belongs to the
    /// user's machine rather than to this kingdom, and picking it up would let
    /// a file they forgot about instruct every plan in every project.
    #[test]
    fn guidance_above_the_kingdom_is_left_alone() {
        let outer = temp();
        let root = outer.join("kingdom");
        let city = root.join("city");

        write(&outer, "AGENTS.md", "somebody else's rules");
        write(&city, "AGENTS.md", "city rules");

        let found = discover_guidance(&city, &root);

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].body, "city rules");

        std::fs::remove_dir_all(&outer).ok();
    }
}
