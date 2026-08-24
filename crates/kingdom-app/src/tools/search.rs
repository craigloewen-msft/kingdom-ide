//! Searching the workspace for a pattern.
//!
//! The court's way of finding something it cannot yet name a path for. It walks
//! the plan's workspace honouring `.gitignore`, which is the whole reason this
//! is a tool and not a `grep` command: an unfiltered walk of a real project
//! returns `target/` and `node_modules/` first and fills the model's context
//! with build output, so the useful matches never arrive.
//!
//! The walk is rooted at [`Workshop::resolve`]'s answer, never at a raw path
//! from the model. A search is a read of every file it touches, so an
//! unresolved `path` here leaks a neighbouring city one line at a time -- the
//! quietest possible version of crossing the boundary.

use super::{Refusal, Tool, Workshop};
use globset::{Glob, GlobMatcher};
use ignore::WalkBuilder;
use kingdom_core::DeedOutcome;
use regex::Regex;
use serde_json::{json, Value};
use std::fmt::Write as _;
use std::io::{BufRead, Read};
use std::path::{Path, PathBuf};

/// How many matching lines one call returns.
///
/// A cap on the *request to the model*, not on the search. Fifty matches is
/// enough to see a pattern's shape; past that the answer is not "more lines"
/// but a narrower query, which is what the truncation note asks for.
const DEFAULT_MAX_RESULTS: usize = 50;

/// Files larger than this are skipped rather than scanned.
///
/// A ten-megabyte file that survived the ignore rules is a lockfile, a fixture
/// or a bundle. Scanning it costs seconds and its matches are never the ones
/// the court wanted.
const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;

/// A ceiling on directory entries visited, so a search cannot become a hang.
///
/// A worktree with a stray `~/Downloads` symlink or a vendored monorepo will
/// walk for minutes, and the King is sitting watching a plan that looks stuck.
/// Stopping and *saying so* turns that into one more turn.
const MAX_ENTRIES_VISITED: usize = 100_000;

/// Directories pruned from the walk entirely.
///
/// Belt and braces over `.gitignore`: these are ignored in most projects but
/// not all, and a repository that happens to commit its `vendor/` is not a
/// reason to bury every search under it. `.git` is here because hidden entries
/// are deliberately *not* filtered -- the court has honest business in
/// `.github/` and dotfiles.
const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    ".next",
    "__pycache__",
    "vendor",
    ".venv",
    "venv",
];

/// Extensions taken as binary without opening the file.
///
/// The sniff below is correct on its own; this list only spares it an open and
/// a read per artefact, which is the difference between a fast search and a
/// slow one in a tree full of compiled output.
const BINARY_EXTS: &[&str] = &[
    "wasm", "so", "dylib", "dll", "a", "o", "obj", "exe", "bin", "rlib", "lib", "zip", "gz", "tar",
    "tgz", "bz2", "xz", "7z", "zst", "png", "jpg", "jpeg", "gif", "webp", "ico", "bmp", "tiff",
    "pdf", "mp3", "mp4", "wav", "mov", "webm", "ttf", "otf", "woff", "woff2", "class", "jar",
    "pyc", "db", "sqlite", "sqlite3",
];

pub struct Search;

#[async_trait::async_trait]
impl Tool for Search {
    fn name(&self) -> &'static str {
        "search"
    }

    fn description(&self) -> String {
        "Search the workspace for a regular expression. Returns matching lines \
         with their file and line number, skipping anything gitignored. Search \
         the whole workspace by default; pass `path` only to narrow to a \
         subdirectory or a single file."
            .to_string()
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["pattern"],
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Regular expression to look for."
                },
                "path": {
                    "type": "string",
                    "description": "File or subdirectory to search, relative to the workspace root. Defaults to the whole workspace."
                },
                "include": {
                    "type": "string",
                    "description": "Only search files matching this glob. A pattern without `/` matches the file name at any depth (e.g. \"*.rs\"); one with `/` matches the path relative to the search root (e.g. \"src/**/*.rs\")."
                },
                "exclude": {
                    "type": "string",
                    "description": "Skip files matching this glob, same rules as `include` (e.g. \"*_test.rs\")."
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum matching lines to return. Default: 50."
                }
            }
        })
    }

    async fn run(&self, input: Value, shop: &Workshop) -> DeedOutcome {
        let Some(pattern) = input.get("pattern").and_then(Value::as_str) else {
            return Refusal::BadArguments {
                tool: "search".to_string(),
                detail: "no `pattern` was given".to_string(),
            }
            .into();
        };

        let re = match Regex::new(pattern) {
            Ok(re) => re,
            Err(e) => {
                return Refusal::BadArguments {
                    tool: "search".to_string(),
                    detail: format!("`{pattern}` is not a valid regular expression: {e}"),
                }
                .into()
            }
        };

        // Even the default root goes through `resolve`, so there is exactly one
        // way a path becomes a walk and no branch that could grow past it.
        let root = match shop.resolve(".") {
            Ok(root) => root,
            Err(refusal) => return refusal.into(),
        };
        let search_path = match input.get("path").and_then(Value::as_str) {
            Some(p) => match shop.resolve(p) {
                Ok(p) => p,
                Err(refusal) => return refusal.into(),
            },
            None => root.clone(),
        };

        let include = match glob(input.get("include").and_then(Value::as_str)) {
            Ok(g) => g,
            Err(refusal) => return refusal.into(),
        };
        let exclude = match glob(input.get("exclude").and_then(Value::as_str)) {
            Ok(g) => g,
            Err(refusal) => return refusal.into(),
        };

        let max_results = input
            .get("max_results")
            .and_then(Value::as_u64)
            .map_or(DEFAULT_MAX_RESULTS, |n| (n as usize).max(1));

        let hunt = Hunt {
            re,
            root,
            search_path,
            include,
            exclude,
            max_results,
        };

        // The walk is synchronous and disk-bound. Left on the async runtime it
        // would block a worker thread for as long as the tree takes, stalling
        // every other plan's model call on the same executor.
        match tokio::task::spawn_blocking(move || hunt.run()).await {
            Ok(output) => DeedOutcome::Done { output },
            Err(e) => Refusal::Refused(format!("the search did not finish: {e}")).into(),
        }
    }
}

/// One search, with everything it needs owned outright.
///
/// Owned rather than borrowed because it crosses onto the blocking pool, where
/// nothing may borrow from the caller's frame.
struct Hunt {
    re: Regex,
    /// The workspace root, used only to make reported paths relative -- an
    /// absolute path in every line wastes tokens and tells the model nothing it
    /// can use in a later call.
    root: PathBuf,
    search_path: PathBuf,
    include: Option<GlobFilter>,
    exclude: Option<GlobFilter>,
    max_results: usize,
}

impl Hunt {
    fn run(self) -> String {
        let walker = WalkBuilder::new(&self.search_path)
            .hidden(false)
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .filter_entry(|entry| {
                !(entry.file_type().is_some_and(|ft| ft.is_dir())
                    && entry
                        .file_name()
                        .to_str()
                        .is_some_and(|name| SKIP_DIRS.contains(&name)))
            })
            .build();

        let mut results: Vec<String> = Vec::new();
        let mut visited = 0usize;
        let mut walk_truncated = false;

        for entry in walker {
            visited += 1;
            if visited > MAX_ENTRIES_VISITED {
                walk_truncated = true;
                break;
            }

            let Ok(entry) = entry else { continue };
            let path = entry.path();
            if entry.file_type().is_none_or(|ft| ft.is_dir()) {
                continue;
            }

            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let relative = path.strip_prefix(&self.search_path).unwrap_or(path);

            if self
                .include
                .as_ref()
                .is_some_and(|g| !g.matches(name, relative))
            {
                continue;
            }
            if self
                .exclude
                .as_ref()
                .is_some_and(|g| g.matches(name, relative))
            {
                continue;
            }

            if self.scan(path, &mut results) {
                break;
            }
        }

        self.report(&results, walk_truncated)
    }

    /// Scans one file, returning `true` when the cap is reached and the walk
    /// should stop.
    fn scan(&self, path: &Path, results: &mut Vec<String>) -> bool {
        if std::fs::metadata(path).is_ok_and(|m| m.len() > MAX_FILE_BYTES) {
            return false;
        }
        if is_binary(path) {
            return false;
        }
        let Ok(file) = std::fs::File::open(path) else {
            return false;
        };

        let shown = path.strip_prefix(&self.root).unwrap_or(path).display();
        // Streamed line by line, never read whole: a search is run against the
        // whole tree, and holding each file's full contents in memory to look
        // at one line at a time is how a search of a large repo becomes an
        // out-of-memory kill.
        let reader = std::io::BufReader::new(file.take(MAX_FILE_BYTES));

        for (index, line) in reader.lines().enumerate() {
            // A non-UTF-8 line ends this file rather than the search: the rest
            // of it is very likely binary the sniff did not catch.
            let Ok(line) = line else { break };
            if self.re.is_match(&line) {
                results.push(format!("{shown}:{}: {}", index + 1, line.trim_end()));
                if results.len() >= self.max_results {
                    return true;
                }
            }
        }
        false
    }

    /// Turns matches into the answer the model sees.
    ///
    /// A search that found nothing is still a search that *ran*, so it is
    /// [`DeedOutcome::Done`] with "no matches" -- reporting it as a refusal
    /// would tell the model to fix its call when the honest finding is that the
    /// thing it looked for is not there.
    fn report(&self, results: &[String], walk_truncated: bool) -> String {
        let mut out = if results.is_empty() {
            "No matches.".to_string()
        } else {
            results.join("\n")
        };

        if results.len() >= self.max_results {
            let _ = write!(
                out,
                "\n\n[Stopped at {} matches; there may be more. Narrow the \
                 pattern, or pass `path` or `include`.]",
                self.max_results
            );
        }
        if walk_truncated {
            let _ = write!(
                out,
                "\n\n[Stopped after {MAX_ENTRIES_VISITED} files; part of the \
                 workspace was never looked at. Narrow with `path` or \
                 `include`.]"
            );
        }
        out
    }
}

fn glob(pattern: Option<&str>) -> Result<Option<GlobFilter>, Refusal> {
    pattern.map(GlobFilter::new).transpose()
}

/// A glob with gitignore-style semantics.
///
/// A pattern without `/` matches the file *name* at any depth, because a model
/// asking for `*.rs` means every Rust file and not only the ones in the root --
/// plain glob semantics answer that with nothing, which reads as "there is no
/// Rust here" and sends the model looking in the wrong place.
struct GlobFilter {
    matcher: GlobMatcher,
    anchored: bool,
}

impl GlobFilter {
    fn new(pattern: &str) -> Result<Self, Refusal> {
        let glob = Glob::new(pattern).map_err(|e| Refusal::BadArguments {
            tool: "search".to_string(),
            detail: format!("`{pattern}` is not a valid glob: {e}"),
        })?;
        Ok(Self {
            matcher: glob.compile_matcher(),
            anchored: pattern.contains('/'),
        })
    }

    fn matches(&self, file_name: &str, relative: &Path) -> bool {
        if self.anchored {
            self.matcher.is_match(relative)
        } else {
            self.matcher.is_match(file_name)
        }
    }
}

fn is_binary(path: &Path) -> bool {
    if path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| BINARY_EXTS.contains(&ext.to_ascii_lowercase().as_str()))
    {
        return true;
    }
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let mut reader = std::io::BufReader::with_capacity(8192, file);
    reader.fill_buf().is_ok_and(|buf| buf.contains(&0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kingdom_core::Workspace;

    async fn search(root: &Path, input: Value) -> String {
        let shop = Workshop::new(Workspace::in_place(root.to_str().unwrap()));
        match Search.run(input, &shop).await {
            DeedOutcome::Done { output } => output,
            DeedOutcome::Refused { reason } => panic!("refused: {reason}"),
        }
    }

    /// The reason this is a tool and not `grep`. A build directory holds
    /// thousands of files that match anything, and an unfiltered search buries
    /// the one useful hit under them and spends the model's context doing it.
    #[tokio::test]
    async fn gitignored_files_are_not_searched() {
        let dir = tempfile::tempdir().unwrap();
        // A bare `.git` marker: `ignore` only applies `.gitignore` inside a
        // repository, and every real workspace is a worktree of one.
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".gitignore"), "generated/\n").unwrap();
        std::fs::write(dir.path().join("real.rs"), "let needle = 1;\n").unwrap();
        std::fs::create_dir(dir.path().join("generated")).unwrap();
        std::fs::write(dir.path().join("generated/out.rs"), "let needle = 2;\n").unwrap();

        let out = search(dir.path(), json!({"pattern": "needle"})).await;

        assert!(out.contains("real.rs"), "{out}");
        assert!(!out.contains("out.rs"), "gitignored output must not appear: {out}");
    }

    /// Truncation has to be *said*. A model handed exactly the cap in silence
    /// concludes it has seen every match and reasons from an incomplete
    /// picture; told it was cut off, it narrows and asks again.
    #[tokio::test]
    async fn hitting_the_cap_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let body = "needle\n".repeat(20);
        std::fs::write(dir.path().join("many.txt"), body).unwrap();

        let out = search(dir.path(), json!({"pattern": "needle", "max_results": 5})).await;

        assert_eq!(out.lines().filter(|l| l.contains("many.txt")).count(), 5);
        assert!(out.contains("Stopped at 5 matches"), "{out}");
    }

    /// Finding nothing is an answer, not a failure -- `Done` means the tool
    /// ran. Refusing here would have the model correct a call that was right.
    #[tokio::test]
    async fn no_matches_is_still_done() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "nothing of interest\n").unwrap();

        assert_eq!(search(dir.path(), json!({"pattern": "needle"})).await, "No matches.");
    }

    /// The boundary, exercised through `path` -- a search reads every file it
    /// walks, so an unresolved root leaks a neighbouring checkout line by line.
    #[tokio::test]
    async fn a_search_path_outside_the_workspace_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "needle\n").unwrap();

        let shop = Workshop::new(Workspace::in_place(dir.path().to_str().unwrap()));
        let outcome = Search
            .run(
                json!({"pattern": "needle", "path": outside.path().to_str().unwrap()}),
                &shop,
            )
            .await;

        assert!(
            matches!(outcome, DeedOutcome::Refused { .. }),
            "searching outside the workspace must be refused: {outcome:?}"
        );
    }
}
