//! The charter: everything the court is told before it is asked anything.
//!
//! Server-only, and lifted out of the provider on purpose. The system prompt
//! used to be built inside `copilot.rs`, which made it *Copilot's* prompt: a
//! second provider would have had to reinvent it, and the two would have
//! drifted the first time either was touched. What a model is told about the
//! work is content, not transport, so it is assembled once here and every
//! provider renders the same words.
//!
//! Named on-metaphor, unlike its neighbours in `llm/`. The precedent there --
//! `llm`, `tools` -- is that *plumbing* keeps its plain name; this is not
//! plumbing. A charter is the document that grants and limits powers, which is
//! exactly what this is: it tells the court where it is standing, what it may
//! touch, and what the project expects of it.

use super::CityBrief;
use kingdom_core::Remit;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Guidance filenames, in order of preference within one directory.
const GUIDANCE_NAMES: &[&str] = &["AGENTS.md", "AGENT.md"];

/// How much project guidance is worth sending.
///
/// A cap rather than trust, because the cost is per *round*, not per turn: the
/// whole charter is resent on every pass of a loop that may run 24 times, so a
/// megabyte of `AGENTS.md` is a bill rather than merely a long prompt. Generous
/// enough that no honest guidance file comes near it.
const MOST_GUIDANCE: usize = 64 * 1024;

/// Everything the court is told about the work, assembled once per turn.
///
/// Held as parts rather than one finished string so a provider can place them
/// as its own API prefers -- and so the pieces stay testable individually.
#[derive(Debug, Clone, Default)]
pub struct Charter {
    /// The project. Kept as the brief rather than as rendered prose because a
    /// provider occasionally needs a *fact* from it -- the city's name for a
    /// fallback headline -- and re-parsing it out of the rendering would be
    /// absurd.
    pub city: CityBrief,
    /// Where the court is standing, and whether it is isolated.
    pub workspace: String,
    /// What it may do, and what it must not.
    pub remit: String,
    /// Every `AGENTS.md` found on the way up from the workspace.
    pub guidance: Vec<Guidance>,
}

/// One guidance file, and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Guidance {
    pub path: String,
    pub body: String,
}

impl Charter {
    /// Assembles the charter for one turn.
    ///
    /// `root` bounds the walk: guidance is gathered from the workspace up to
    /// the kingdom root and no further, so a stray `AGENTS.md` in the King's
    /// home directory cannot silently instruct every plan in every project.
    pub fn assemble(
        city: &CityBrief,
        workspace: &kingdom_core::Workspace,
        remit: Remit,
        approved: bool,
        root: &Path,
    ) -> Self {
        Self {
            city: city.clone(),
            workspace: workspace_block(workspace),
            remit: remit_block(remit, approved),
            guidance: discover_guidance(Path::new(&workspace.path), root),
        }
    }

    /// The charter as one system prompt.
    ///
    /// Order carries reasoning and is not arbitrary. The remit comes before the
    /// testing directive, which comes before project guidance: a project's own
    /// rules are the most specific thing here, so they arrive last and win any
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
        out.push_str(&self.remit);

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
        out.push_str(MERMAID);

        out
    }
}

const PREAMBLE: &str = "You are a senior software engineer helping with one project.";

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

const MERMAID: &str = "The chamber renders Markdown mermaid code fences as diagrams; prefer \
     them when a diagram would help. Wrap a node label in double quotes when it contains \
     parentheses or quotes, so Mermaid does not read the punctuation as syntax.";

/// Where the court is standing.
///
/// The court is not told this anywhere else. `begin_plan` records the workspace
/// as a [`kingdom_core::NoteKind::Workspace`] note, and `Plan::turns`
/// deliberately withholds notes from the model -- so without this block an
/// agent working in a worktree at `<city>/.kingdom/<uuid>` has no idea it is
/// not in the project's own checkout, and will happily describe its work as
/// having changed the King's files.
fn workspace_block(workspace: &kingdom_core::Workspace) -> String {
    let mut out = format!("Working directory: {}\n", workspace.path);
    match (&workspace.branch, workspace.is_isolated()) {
        (Some(branch), true) => out.push_str(&format!(
            "This is an isolated worktree on branch {branch}. It is yours: the King's own \
             checkout is elsewhere and is not affected by what you do here.\n"
        )),
        _ => out.push_str(
            "This is the project directory itself, with no isolation. Anything you change \
             here changes the King's own checkout.\n",
        ),
    }
    out
}

/// What the court may do, in the words it is told it.
fn remit_block(remit: Remit, approved: bool) -> String {
    match remit {
        Remit::Survey => SURVEY.to_string(),
        Remit::Counsel => COUNSEL.to_string(),
        Remit::Full if approved => format!("{FULL}\n\n{CARRYING_OUT}"),
        Remit::Full => FULL.to_string(),
    }
}

const SURVEY: &str = "\nYou were sent to answer one question and report back. You can read \
     and search, and that is all: you cannot run commands, edit files, or send errands of \
     your own. Answer what you were asked, concretely, citing the files you looked at.";

/// The counsel block: the heart of this whole arrangement.
///
/// Note what it says about `bash`, and why it says it. The tool list is not a
/// sandbox -- `Workshop::root` is explicit that the path boundary does not
/// contain a shell -- so a command that names an absolute path can write
/// anywhere. Withholding `bash` would buy a guarantee Kingdom cannot keep while
/// costing the court `git log`, `cargo tree`, and running the failing test it
/// is proposing to fix. So the limit is stated as what it is: a boundary the
/// court is trusted to keep. Pretending otherwise would be worse, because the
/// King would believe in a fence that is not there.
const COUNSEL: &str = "\nYou are drawing up a plan, not carrying it out. Read, search, and \
     run whatever you need in order to understand the work -- but change nothing. No edits \
     to files, no commits, nothing written into the project.\n\n\
     You have `bash`, and it is not fenced in: a command that names an absolute path can \
     write anywhere on this machine. That boundary is one you are trusted to keep, not one \
     Kingdom enforces. Use it to look -- `git log`, `cargo tree`, running the tests to see \
     which fail -- and never to change.\n\n\
     When you know what should be done, call `propose_plan` with a title and the plan \
     itself. Say what you would change, in which files, and why; say what you checked and \
     what you are assuming. The King reads it and either starts you on it or sends back \
     changes, and you have no hands until he does.\n\n\
     If he asks you to change something directly, explain that you must put a plan to him \
     first.";

const FULL: &str = "\nYou have tools and are working in the directory above. Use them: read \
     before you change, and check your work by running it rather than by assuming. The file \
     list is a starting point, not the whole project. When you have finished, reply with \
     what you did and what it means for the reader -- concisely, and without repeating the \
     output of commands they can already see.";

const CARRYING_OUT: &str = "You are carrying out a plan the King approved. It is above, in \
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
        // belongs to the King's machine, not to this kingdom.
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
            "kingdom-charter-{}-{}",
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

    /// The walk stops at the kingdom root. Guidance above it belongs to the
    /// King's machine rather than to this kingdom, and picking it up would let
    /// a file he forgot about instruct every plan in every project.
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
