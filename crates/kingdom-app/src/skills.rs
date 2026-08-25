//! Skills: instructions a project keeps on disk for an agent to pick up.
//!
//! Server-only, for the same reason `llm/` and `tools/` are -- it walks the
//! filesystem, which the wasm bundle cannot do.
//!
//! A skill is a directory containing a `SKILL.md`: YAML frontmatter naming it
//! and describing when it applies, then a body of instructions. Kingdom finds
//! them, lists their names and descriptions in the system prompt, and hands the
//! body over when the model calls the `skill` tool. The catalogue is metadata
//! only -- the body is fetched on demand, so a project with twenty skills
//! spends twenty lines of prompt rather than twenty documents.
//!
//! Ported from Phoenix IDE's `phoenix-skills` crate, whose conventions
//! (`.claude/skills/`, `.agents/skills/`, the frontmatter keys, `$ARGUMENTS`)
//! are the ecosystem's rather than Phoenix's own. Kingdom reads the same
//! directories so a project already carrying skills works here unchanged.
//!
//! Phoenix's built-in skills are deliberately not ported: it embeds a set in
//! its binary with `rust-embed` and extracts them at startup, and Kingdom has
//! none to embed. That is why [`Skill`] carries a plain path rather than
//! Phoenix's `SkillSource` enum.

use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

/// Where skills live, relative to any directory on the way up.
const SKILL_DIRS: &[&str] = &[".claude/skills", ".agents/skills"];

/// One skill found on disk: what it is called, when to use it, and where it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    /// The name the model invokes, namespaced for a sub-skill: `allium:distill`.
    pub name: String,
    /// When this skill applies, from the frontmatter. Shown in the catalogue.
    pub description: String,
    /// What arguments it expects, if it said. Not shown to the model -- kept
    /// because the frontmatter carries it and dropping it here would mean
    /// re-reading the file to get it back.
    pub argument_hint: Option<String>,
    /// Absolute path to the `SKILL.md` itself.
    pub path: PathBuf,
}

impl Skill {
    /// The directory holding this skill, which is what the model is told so it
    /// can read the skill's companion files.
    pub fn dir(&self) -> String {
        self.path
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default()
    }

    /// How the catalogue names this skill's location.
    pub fn display_location(&self) -> String {
        format!("(`{}`)", self.path.display())
    }
}

/// A skill's body, ready to hand to the model.
#[derive(Debug, Clone)]
pub struct SkillInvocation {
    pub name: String,
    /// The instructions: frontmatter stripped, base directory prepended,
    /// arguments substituted.
    pub body: String,
}

/// The frontmatter fields worth reading.
struct Frontmatter {
    name: String,
    description: String,
    argument_hint: Option<String>,
}

/// Reads `name`, `description` and optional `argument-hint` from a `SKILL.md`.
///
/// A file missing either required field yields `None` and is skipped rather
/// than guessed at: a skill with no description is one the model cannot choose
/// between, and inventing one from the filename would be worse than omitting it.
fn parse_frontmatter(content: &str) -> Option<Frontmatter> {
    let body = content.strip_prefix("---\n")?;
    let end = body
        .find("\n---\n")
        // Frontmatter closing the file with no trailing newline.
        .or_else(|| body.find("\n---").filter(|&i| i + 4 == body.len()))?;
    let frontmatter = &body[..end];

    let mut name = None;
    let mut description = None;
    let mut argument_hint = None;

    for line in frontmatter.lines() {
        if let Some(value) = line.strip_prefix("name:") {
            name = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("description:") {
            description = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("argument-hint:") {
            let hint = value.trim().to_string();
            if !hint.is_empty() {
                argument_hint = Some(hint);
            }
        }
    }

    Some(Frontmatter {
        name: name?,
        description: description?,
        argument_hint,
    })
}

/// What has been seen already, so the same skill is never listed twice.
///
/// Three separate checks, because there are three separate ways to meet a
/// duplicate: the same file reached by a symlink, a copy of it somewhere else,
/// and a genuinely different file claiming a name that is taken. The first one
/// found wins in every case, and the walk is ordered nearest-first, so the
/// closest definition of a name is the one that survives.
#[derive(Default)]
struct Seen {
    names: HashSet<String>,
    paths: HashSet<PathBuf>,
    contents: HashSet<u64>,
    dirs: HashSet<PathBuf>,
}

/// Gathers every skill in one skills directory.
///
/// Recurses one level of naming: a skill containing its own `skills/`
/// subdirectory contributes `parent:child`, which is how a bundle of related
/// skills stays addressable without flattening its names into the global set.
fn collect(dir: &Path, namespace: &str, found: &mut Vec<Skill>, seen: &mut Seen) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let manifest = path.join("SKILL.md");
        if !manifest.is_file() {
            continue;
        }

        let canonical = std::fs::canonicalize(&manifest).unwrap_or_else(|_| manifest.clone());
        if !seen.paths.insert(canonical) {
            continue;
        }

        let Ok(content) = std::fs::read_to_string(&manifest) else {
            continue;
        };

        if !seen.contents.insert(hash(&content)) {
            continue;
        }

        let Some(front) = parse_frontmatter(&content) else {
            continue;
        };

        let name = if namespace.is_empty() {
            front.name.clone()
        } else {
            format!("{namespace}:{}", front.name)
        };

        if seen.names.insert(name.clone()) {
            found.push(Skill {
                name: name.clone(),
                description: front.description,
                argument_hint: front.argument_hint,
                path: manifest,
            });
        }

        let nested = path.join("skills");
        if nested.is_dir() {
            collect(&nested, &name, found, seen);
        }
    }
}

/// Scans one directory's `.claude/skills` and `.agents/skills`, if either is there.
fn scan(base: &Path, found: &mut Vec<Skill>, seen: &mut Seen) {
    for subdir in SKILL_DIRS {
        let dir = base.join(subdir);
        if !dir.is_dir() {
            continue;
        }
        let canonical = std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
        if !seen.dirs.insert(canonical) {
            continue;
        }
        collect(&dir, "", found, seen);
    }
}

/// Every skill available to an agent working in `from`, sorted by name.
///
/// Four passes, in this order, because order is what decides which definition
/// of a repeated name wins:
///
/// 1. **Up the tree from the workspace.** Nearest first, so a project's own
///    version of a skill beats the one its parent directory offers.
/// 2. **One level down.** The case where the kingdom root itself is opened: the
///    cities are children, and their skills would otherwise be invisible.
/// 3. **`$HOME`.** Explicitly, because the workspace may be on a different
///    mount entirely and the walk up would never reach it.
///
/// The walk is unbounded, matching Phoenix. A skill is opt-in in a way guidance
/// is not -- the model has to name one to run it, and it only knows the names
/// in the catalogue -- so a stray skills directory high up the tree offers a
/// capability rather than issuing an instruction.
pub fn discover(from: &Path) -> Vec<Skill> {
    discover_with_home(from, dirs_home().as_deref())
}

/// [`discover`], with `$HOME` named explicitly.
///
/// Tests use this to point at a temporary directory instead of the real home,
/// which would otherwise make the result depend on whoever is running them.
pub fn discover_with_home(from: &Path, home: Option<&Path>) -> Vec<Skill> {
    let mut found = Vec::new();
    let mut seen = Seen::default();

    let mut here = Some(from.to_path_buf());
    while let Some(dir) = here {
        scan(&dir, &mut found, &mut seen);
        here = dir.parent().map(Path::to_path_buf);
    }

    if let Ok(children) = std::fs::read_dir(from) {
        for child in children.flatten() {
            if child.path().is_dir() {
                scan(&child.path(), &mut found, &mut seen);
            }
        }
    }

    if let Some(home) = home {
        scan(home, &mut found, &mut seen);
    }

    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

/// The user's home directory, or `None` where the environment does not say.
fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

/// Looks a skill up and prepares its body for the model.
///
/// The base directory is prepended because a skill's instructions routinely
/// point at files beside it -- "see `references/api.md`" is only actionable if
/// the model knows where `references/` is.
pub fn invoke(name: &str, arguments: &str, skills: &[Skill]) -> Result<SkillInvocation, String> {
    let Some(skill) = skills.iter().find(|s| s.name == name) else {
        let available: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        return Err(format!(
            "There is no skill called \"{name}\". Available: {}",
            if available.is_empty() {
                "none".to_string()
            } else {
                available.join(", ")
            }
        ));
    };

    let raw = std::fs::read_to_string(&skill.path)
        .map_err(|e| format!("The skill \"{name}\" could not be read: {e}"))?;

    let body = strip_frontmatter(&raw);
    let body = format!("Base directory for this skill: {}\n\n{body}", skill.dir());

    Ok(SkillInvocation {
        name: name.to_string(),
        body: substitute(&body, arguments),
    })
}

/// Everything after the frontmatter block, or the whole file if there is none.
fn strip_frontmatter(content: &str) -> String {
    let trimmed = content.trim_start();
    let Some(after_open) = trimmed.strip_prefix("---") else {
        return content.to_string();
    };
    match after_open.find("\n---") {
        Some(end) => after_open[end + 4..].trim_start_matches('\n').to_string(),
        None => content.to_string(),
    }
}

/// Puts the caller's arguments into the body.
///
/// Positional placeholders are substituted before `$ARGUMENTS`, because doing
/// it the other way round would expand `$ARGUMENTS` inside `$ARGUMENTS[1]` and
/// leave a `[1]` stranded after it. A skill with no placeholder at all gets the
/// arguments appended, so passing them is never silently ignored.
fn substitute(body: &str, arguments: &str) -> String {
    if arguments.is_empty() {
        return body.to_string();
    }

    if !body.contains("$ARGUMENTS") {
        return format!("{body}\nARGUMENTS: {arguments}");
    }

    let mut out = body.to_string();
    for (i, token) in arguments.split_whitespace().enumerate() {
        let n = i + 1;
        out = out
            .replace(&format!("$ARGUMENTS[{n}]"), token)
            .replace(&format!("${n}"), token);
    }
    out.replace("$ARGUMENTS", arguments)
}

fn hash(text: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory of this test's own.
    ///
    /// The counter is not belt-and-braces. Every test in this module runs in
    /// one process, so the pid is shared, and two threads calling this in the
    /// same clock tick would otherwise get the same path -- which shows up as
    /// one test finding another's skills, only sometimes, and only under the
    /// full suite. That is exactly the failure this replaced.
    fn temp() -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);

        let dir = std::env::temp_dir().join(format!(
            "kingdom-skills-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        // A leftover from a previous run would be another test's skills as far
        // as this one is concerned.
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_skill(base: &Path, subdir: &str, dir_name: &str, name: &str, description: &str) {
        let dir = base.join(subdir).join(dir_name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {description}\n---\n\n# {name}\nbody here\n"),
        )
        .unwrap();
    }

    /// Both conventions are read, and the result is ordered by name so the
    /// prompt is byte-stable turn to turn. An unstable catalogue would
    /// invalidate the provider's prompt cache on every round for no reason.
    #[test]
    fn finds_skills_in_both_conventions_sorted() {
        let root = temp();
        write_skill(
            &root,
            ".claude/skills",
            "zebra",
            "zebra",
            "last alphabetically",
        );
        write_skill(
            &root,
            ".agents/skills",
            "alpha",
            "alpha",
            "first alphabetically",
        );

        let found = discover_with_home(&root, None);

        assert_eq!(found.len(), 2, "{found:#?}");
        assert_eq!(found[0].name, "alpha");
        assert_eq!(found[1].name, "zebra");

        std::fs::remove_dir_all(&root).ok();
    }

    /// The nearest definition of a name wins, which is what lets a project
    /// override a skill it inherits from the directory above it.
    #[test]
    fn the_nearest_skill_of_a_name_wins() {
        let root = temp();
        let project = root.join("project");
        std::fs::create_dir_all(&project).unwrap();

        write_skill(&root, ".claude/skills", "build", "build", "the general one");
        write_skill(
            &project,
            ".claude/skills",
            "build",
            "build",
            "the project's own",
        );

        let found = discover_with_home(&project, None);

        assert_eq!(found.len(), 1, "one name, one skill: {found:#?}");
        assert_eq!(found[0].description, "the project's own");

        std::fs::remove_dir_all(&root).ok();
    }

    /// A skill bundle names its children after itself, so two bundles may each
    /// carry a `distill` without colliding.
    #[test]
    fn a_sub_skill_is_named_after_its_parent() {
        let root = temp();
        write_skill(&root, ".claude/skills", "allium", "allium", "the bundle");
        let nested = root.join(".claude/skills/allium/skills");
        write_skill(&nested, "", "distill", "distill", "the sub-skill");

        let found = discover_with_home(&root, None);

        let names: Vec<&str> = found.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["allium", "allium:distill"], "{found:#?}");

        std::fs::remove_dir_all(&root).ok();
    }

    /// A skill with no description is skipped rather than listed blank: the
    /// description is the only thing the model chooses between skills on.
    #[test]
    fn a_skill_without_a_description_is_not_offered() {
        let root = temp();
        let dir = root.join(".claude/skills/nameless");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), "---\nname: nameless\n---\n\nbody\n").unwrap();

        assert!(discover_with_home(&root, None).is_empty());

        std::fs::remove_dir_all(&root).ok();
    }

    /// Nothing on disk means no catalogue at all, rather than an empty heading.
    #[test]
    fn a_project_with_no_skills_finds_none() {
        let root = temp();
        assert!(discover_with_home(&root, None).is_empty());
        std::fs::remove_dir_all(&root).ok();
    }

    /// The body reaches the model with its frontmatter gone and its own
    /// location prepended, which is what makes "see `references/api.md`"
    /// followable.
    #[test]
    fn invoking_a_skill_strips_frontmatter_and_names_its_directory() {
        let root = temp();
        write_skill(&root, ".claude/skills", "build", "build", "builds it");
        let skills = discover_with_home(&root, None);

        let invocation = invoke("build", "", &skills).unwrap();

        assert!(
            !invocation.body.contains("description:"),
            "{}",
            invocation.body
        );
        assert!(invocation.body.contains("# build"));
        assert!(invocation.body.contains(&format!(
            "Base directory for this skill: {}",
            root.join(".claude/skills/build").display()
        )));

        std::fs::remove_dir_all(&root).ok();
    }

    /// An unknown name lists what there is, so the model's next call can be
    /// right. A bare "not found" earns a retry of the same guess.
    #[test]
    fn an_unknown_skill_names_the_ones_that_exist() {
        let root = temp();
        write_skill(&root, ".claude/skills", "build", "build", "builds it");
        let skills = discover_with_home(&root, None);

        let refused = invoke("deploy", "", &skills).unwrap_err();

        assert!(refused.contains("deploy"), "{refused}");
        assert!(refused.contains("build"), "{refused}");

        std::fs::remove_dir_all(&root).ok();
    }

    /// Positional placeholders survive the full `$ARGUMENTS` substitution that
    /// follows them -- the ordering bug this pins is silent and produces a
    /// stranded `[1]`.
    #[test]
    fn positional_arguments_are_substituted_before_the_whole() {
        let out = substitute("first=$ARGUMENTS[1] all=$ARGUMENTS", "one two");
        assert_eq!(out, "first=one all=one two");
    }

    /// Arguments passed to a skill that names no placeholder are appended
    /// rather than dropped on the floor.
    #[test]
    fn arguments_are_never_silently_discarded() {
        let out = substitute("a skill that takes no arguments", "but got some");
        assert!(out.contains("ARGUMENTS: but got some"), "{out}");
    }
}
