//! Reading a codebase off disk.
//!
//! Ignore rules are honoured, and a short list of generated directories is
//! excluded whatever the ignore files say: drawing `node_modules` tells you
//! about npm, not about the codebase.
//!
//! References are counted by resolving imports as paths. An import line names
//! a place — `./engine`, `crate::engine::Runner`, `app.engine`,
//! `"lib/engine.h"` — and every language writes that place as a path through
//! the folder tree, whatever punctuation it uses to separate the segments.
//! Normalising the separators is enough to look the target up in the tree the
//! scan already has, which costs nothing, needs no toolchain installed, and is
//! the same rule in every language.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use ignore::WalkBuilder;

use crate::build::model::{Category, Metrics, Node, NodeKind, Repository};

/// What a scan yields when it cannot read the disk.
///
/// Upstream this was `anyhow`, which arrived with the CLI this crate does not
/// have. The failures are all filesystem failures, so `std::io` says the same
/// thing without a dependency; [`whichever`] carries the context message that
/// `anyhow::Context` used to add.
pub type Result<T> = std::io::Result<T>;

/// Wraps any error as an [`std::io::Error`] with the context upstream attached
/// through `anyhow::Context`.
///
/// Two of the fallible calls here are not `io` at all -- walking yields
/// `ignore::Error` and stripping a prefix yields `StripPrefixError` -- so the
/// message is what unifies them rather than the type.
fn whichever(error: impl std::fmt::Display, context: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(format!("{context}: {error}"))
}

const MAX_ANALYZED_BYTES: u64 = 2 * 1024 * 1024;
const HARD_IGNORED_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    "node_modules",
    "target",
    "vendor",
    "dist",
    "build",
    ".next",
    ".nuxt",
    "coverage",
    "__pycache__",
];

/// How much of a repository to read.
#[derive(Clone, Copy, Debug)]
pub struct ScanOptions {
    /// The most files to keep. Larger files are kept in preference to smaller
    /// ones, so a truncated scan still shows the shape of the codebase.
    pub max_files: usize,
    /// Whether to include files that ignore rules would normally hide. Hard
    /// exclusions such as `.git` and `target` still apply.
    pub include_ignored: bool,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            max_files: 1_500,
            include_ignored: false,
        }
    }
}

/// Finds the Git repositories at or below `root`, in path order.
///
/// The search stops at each repository boundary, so a repository is never
/// merged into its parent or into a sibling that happens to contain it.
pub fn discover_repositories(root: &Path, max_depth: usize) -> Result<Vec<PathBuf>> {
    let mut repositories = Vec::new();
    discover_recursive(root, 0, max_depth, &mut repositories)?;
    repositories.sort();
    Ok(repositories)
}

fn discover_recursive(
    directory: &Path,
    depth: usize,
    max_depth: usize,
    repositories: &mut Vec<PathBuf>,
) -> Result<()> {
    if directory.join(".git").exists() {
        repositories.push(directory.to_path_buf());
        return Ok(());
    }
    if depth >= max_depth {
        return Ok(());
    }

    let mut children = Vec::new();
    for entry in fs::read_dir(directory)
        .map_err(|error| whichever(error, format!("could not inspect {}", directory.display())))?
    {
        let entry = entry.map_err(|error| {
            whichever(error, format!("could not inspect {}", directory.display()))
        })?;
        let file_type = entry.file_type().map_err(|error| {
            whichever(
                error,
                format!("could not inspect {}", entry.path().display()),
            )
        })?;
        if !file_type.is_dir() || file_type.is_symlink() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || HARD_IGNORED_DIRS.contains(&name.as_ref()) {
            continue;
        }
        children.push(entry.path());
    }
    children.sort();
    for child in children {
        discover_recursive(&child, depth + 1, max_depth, repositories)?;
    }
    Ok(())
}

/// Reads a single repository into a tree of [`Node`]s.
pub fn scan_repository(root: &Path, options: &ScanOptions) -> Result<Repository> {
    let root_name = root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("repository")
        .to_owned();
    let mut tree = Node::directory(root_name, PathBuf::new());

    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(false)
        .follow_links(false)
        .parents(!options.include_ignored)
        .ignore(!options.include_ignored)
        .git_ignore(!options.include_ignored)
        .git_global(!options.include_ignored)
        .git_exclude(!options.include_ignored)
        .filter_entry(|entry| {
            entry
                .file_name()
                .to_str()
                .is_none_or(|name| !HARD_IGNORED_DIRS.contains(&name))
        });

    let mut files = Vec::new();
    for entry in builder.build() {
        let entry = entry
            .map_err(|error| whichever(error, format!("could not walk {}", root.display())))?;
        if entry.file_type().is_some_and(|kind| kind.is_file()) {
            let relative = entry
                .path()
                .strip_prefix(root)
                .map_err(|error| {
                    whichever(error, format!("invalid path {}", entry.path().display()))
                })?
                .to_path_buf();
            files.push(relative);
        }
    }
    files.sort();

    let omitted_files = files.len().saturating_sub(options.max_files);
    files.truncate(options.max_files);
    let mut analyzed = Vec::with_capacity(files.len());
    let mut mentions = Vec::with_capacity(files.len());
    for relative in &files {
        let absolute = root.join(relative);
        let (file, imports) = analyze_file(&absolute, relative)?;
        analyzed.push(file);
        mentions.push(imports);
    }

    // Counting has to wait until every file is known: a reference can only be
    // resolved against the whole repository, and the first file read may well
    // be referring to the last.
    for (file, count) in analyzed.iter_mut().zip(count_references(&files, &mentions)) {
        file.metrics.references = count;
    }
    for (relative, file) in files.iter().zip(analyzed) {
        insert_file(&mut tree, relative, file);
    }
    sort_and_aggregate(&mut tree);

    Ok(Repository {
        root: tree,
        source_path: root.to_path_buf(),
        omitted_files,
    })
}

fn insert_file(root: &mut Node, relative: &Path, file: Node) {
    let components: Vec<String> = relative
        .components()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    insert_components(root, &components, file);
}

fn insert_components(parent: &mut Node, components: &[String], file: Node) {
    if components.len() <= 1 {
        parent.children.push(file);
        return;
    }

    let directory_name = &components[0];
    let position = parent
        .children
        .iter()
        .position(|child| child.is_directory() && child.name == *directory_name);
    let index = position.unwrap_or_else(|| {
        let path = parent.relative_path.join(directory_name);
        parent
            .children
            .push(Node::directory(directory_name.clone(), path));
        parent.children.len() - 1
    });
    insert_components(&mut parent.children[index], &components[1..], file);
}

/// Files that stand for the folder they sit in rather than for themselves.
///
/// Nobody writes `use mod` or `import index`. A barrel file is reached by
/// naming its folder, so it is indexed under the folder's path as well as its
/// own — otherwise the most depended-upon file in a codebase, the one every
/// other crate imports by crate name alone, answers to no name at all.
const BARREL_STEMS: &[&str] = &["mod", "index", "__init__", "lib", "main"];

/// Folder names that are packaging rather than part of an import path.
///
/// `use repo_city_map::Thing` names the crate, not `repo-city-map/src/lib.rs`,
/// so the layers a build tool inserts between the two are skipped when a file
/// is indexed by the name others call it.
const PLUMBING_DIRS: &[&str] = &["src", "lib", "source", "app", "pkg", "internal", "crates"];

/// The shortest name worth resolving on its own.
///
/// A single short segment such as `io` or `os` matches too much to mean
/// anything. Longer paths are specific enough to trust however short their
/// last segment is.
const MIN_NAME: usize = 3;

/// The most files one ambiguous import may credit.
///
/// A path that resolves to a handful of candidates is a genuine tie worth
/// splitting. A bare name that matches thirty files is noise, and crediting
/// all thirty is how a heuristic quietly invents a hub.
const MAX_TIED: usize = 4;

/// How many other files refer to each file, by index into `files`.
///
/// An import is a path, not a bag of words, so it is resolved like one:
/// relative specifiers against the folder the importing file sits in, and
/// everything else by matching the longest run of trailing segments that names
/// a real file. Resolution beats recognition — `./engine` and `../lib/engine`
/// land on exactly one file each, where matching bare stems could only shrug
/// and credit both.
fn count_references(files: &[PathBuf], mentions: &[Vec<Vec<String>>]) -> Vec<usize> {
    let index = FileIndex::build(files);

    let mut counts = vec![0usize; files.len()];
    let mut hit = HashSet::new();
    for (source, imported) in mentions.iter().enumerate() {
        hit.clear();
        for specifier in imported {
            for target in index.resolve(specifier, &files[source]) {
                if target != source {
                    hit.insert(target);
                }
            }
        }
        // One file leaning on another counts once, however many times it says
        // so. Otherwise a file re-importing a name in twenty places would look
        // twenty times as depended upon.
        for target in &hit {
            counts[*target] += 1;
        }
    }
    counts
}

/// Every name each file answers to, so an import can be looked up as a path.
///
/// A file is indexed under every trailing run of its own path — `lib`,
/// `map/lib`, `repo-city-map/lib` — and, where the path holds packaging a
/// caller never writes, under the same runs with that packaging removed. An
/// import then resolves by trying its longest form first and shortening until
/// something matches, so a specific path wins over a vague one.
struct FileIndex {
    by_path: HashMap<String, Vec<usize>>,
}

impl FileIndex {
    fn build(files: &[PathBuf]) -> Self {
        let mut by_path: HashMap<String, Vec<usize>> = HashMap::new();
        for (id, file) in files.iter().enumerate() {
            for name in aliases(file) {
                let entry = by_path.entry(name).or_default();
                // A file indexed twice under one name is still one file.
                if entry.last() != Some(&id) {
                    entry.push(id);
                }
            }
        }
        Self { by_path }
    }

    /// The files an import specifier could mean, best guess first.
    fn resolve(&self, specifier: &[String], from: &Path) -> Vec<usize> {
        if specifier.is_empty() {
            return Vec::new();
        }

        // A relative specifier says exactly where to look, so it is resolved
        // against the importing file and matched whole. Falling back to a
        // shorter suffix would throw away the one thing it was precise about.
        if matches!(specifier[0].as_str(), "." | "..") {
            let mut base = from.parent().unwrap_or(Path::new("")).to_path_buf();
            let mut rest = specifier;
            while let Some(step) = rest.first() {
                match step.as_str() {
                    "." => {}
                    ".." => {
                        base.pop();
                    }
                    _ => break,
                }
                rest = &rest[1..];
            }
            let joined: Vec<String> = base
                .components()
                .filter_map(|part| match part {
                    Component::Normal(name) => Some(name.to_string_lossy().to_ascii_lowercase()),
                    _ => None,
                })
                .chain(rest.iter().cloned())
                .collect();
            return self.lookup(&joined.join("/"), true);
        }

        // The longest run of segments that names a real file wins, because it
        // is the most specific reading of the path. Ties in length go to the
        // run nearest the front: a path leads with where to look and trails
        // with what to take, and only the first half names a file.
        for length in (1..=specifier.len()).rev() {
            for start in 0..=specifier.len() - length {
                let found = self.lookup(&specifier[start..start + length].join("/"), start == 0);
                if !found.is_empty() {
                    return found;
                }
            }
        }
        Vec::new()
    }

    /// The files indexed under exactly this path, if it is specific enough.
    ///
    /// A single segment is only trusted at the head of a specifier, where it
    /// is the whole path and means a file at the root of the tree. Further in
    /// it is one name out of several — the `collections` of
    /// `std::collections::HashMap` — and matching it against a file that
    /// happens to share the word invents a dependency on the standard library.
    fn lookup(&self, candidate: &str, at_head: bool) -> Vec<usize> {
        let single = !candidate.contains('/');
        if single && (!at_head || candidate.len() < MIN_NAME) {
            return Vec::new();
        }
        match self.by_path.get(candidate) {
            Some(found) if found.len() <= MAX_TIED => found.clone(),
            _ => Vec::new(),
        }
    }
}

/// Every path an import could name this file by.
fn aliases(file: &Path) -> Vec<String> {
    let mut segments: Vec<String> = file
        .components()
        .filter_map(|part| match part {
            Component::Normal(name) => Some(name.to_string_lossy().to_ascii_lowercase()),
            _ => None,
        })
        .collect();
    let Some(last) = segments.pop() else {
        return Vec::new();
    };

    // A crate imported by name is `repo_city_map`; the folder holding it is
    // `repo-city-map`. One spelling has to win, and the separator is the only
    // difference that is never meaningful.
    let stem = Path::new(&last)
        .file_stem()
        .map(|stem| stem.to_string_lossy().replace('_', "-"))
        .unwrap_or_default();
    for segment in &mut segments {
        *segment = segment.replace('_', "-");
    }

    // A barrel file stands for its folder, so it answers to the folder's path
    // and not to `mod` or `index`.
    if !BARREL_STEMS.contains(&stem.as_str()) {
        segments.push(stem);
    }
    if segments.is_empty() {
        return Vec::new();
    }

    // Both the path as written and the path with build-tool packaging removed,
    // so `crates/repo-city-map/src/lib.rs` answers to `repo-city-map` as well
    // as to `repo-city-map/src`.
    let trimmed: Vec<String> = segments
        .iter()
        .filter(|segment| !PLUMBING_DIRS.contains(&segment.as_str()))
        .cloned()
        .collect();

    let mut names = Vec::new();
    for form in [&segments, &trimmed] {
        for start in 0..form.len() {
            names.push(form[start..].join("/"));
        }
    }
    names.sort();
    names.dedup();
    names
}

/// The paths an import-like line in this file names.
///
/// Only the target of the import is read, not every word on the line. A line
/// says two different kinds of thing at once — where to look, and what to take
/// from there — and only the first is about which file depends on which.
/// `use std::collections::HashMap` names one path; the old reading of it as
/// loose words also offered `collections` and `hashmap` to be matched against
/// whatever the repository happened to contain.
fn imported_paths(text: &str) -> Vec<Vec<String>> {
    let mut paths = Vec::new();
    let mut seen = HashSet::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if !is_import_line(trimmed) {
            continue;
        }
        for specifier in specifiers(trimmed) {
            let segments = split_specifier(&specifier);
            if !segments.is_empty() && seen.insert(segments.clone()) {
                paths.push(segments);
            }
        }
    }
    paths
}

/// The import targets written on one line.
///
/// A quoted or angle-bracketed target is unambiguous, so where a line has one
/// it is taken as written. Otherwise the line is a bare path — Rust's
/// `use a::b`, Python's `from a.b import c`, Java's `import a.b.C` — and the
/// first path-shaped run of characters after the keyword is the target.
fn specifiers(trimmed: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = trimmed;
    while let Some(open) = rest.find(['"', '\'', '<']) {
        let closer = match rest.as_bytes()[open] {
            b'"' => '"',
            b'\'' => '\'',
            _ => '>',
        };
        let after = &rest[open + 1..];
        let Some(close) = after.find(closer) else {
            break;
        };
        let quoted = &after[..close];
        // An angle bracket is only a target on an include line; anywhere else
        // it is a comparison or a generic parameter.
        if closer != '>' || trimmed.starts_with("#include") || trimmed.starts_with("include ") {
            found.push(quoted.to_owned());
        }
        rest = &after[close + 1..];
    }
    if !found.is_empty() {
        return found;
    }

    // A bare path. Skip the leading keywords, then read until the path ends —
    // at a brace, a space, or the `as` of a rename.
    let without_keyword = trimmed
        .split_whitespace()
        .find(|word| {
            !matches!(
                *word,
                "import"
                    | "from"
                    | "use"
                    | "pub"
                    | "mod"
                    | "using"
                    | "extern"
                    | "crate"
                    | "@import"
            )
        })
        .unwrap_or("");
    let path: String = without_keyword
        .chars()
        .take_while(|character| {
            character.is_alphanumeric() || matches!(character, '_' | '-' | '.' | '/' | '\\' | ':')
        })
        .collect();
    if path.is_empty() {
        Vec::new()
    } else {
        vec![path]
    }
}

/// An import target split into the path segments it names.
fn split_specifier(specifier: &str) -> Vec<String> {
    let specifier = specifier.trim_end_matches(';').trim();

    // The leading dots of `./engine` and `../lib/engine` are the whole point
    // of a relative import, and they have to survive being split on `.`.
    let mut lead = Vec::new();
    let mut rest = specifier;
    loop {
        if let Some(tail) = rest.strip_prefix("../") {
            lead.push("..".to_owned());
            rest = tail;
        } else if let Some(tail) = rest.strip_prefix("./") {
            lead.push(".".to_owned());
            rest = tail;
        } else {
            break;
        }
    }

    // A trailing extension is how the file is spelled on disk, not part of the
    // name: `./engine` and `./engine.js` mean the same file. Only a path can
    // carry one — the dots of `app.models.user` are separators.
    let looks_like_path = !lead.is_empty() || rest.contains('/') || rest.contains('\\');
    let stem = match rest.rsplit_once('.') {
        Some((head, tail)) if looks_like_path && !head.is_empty() && tail.len() <= 4 => head,
        _ => rest,
    };

    lead.into_iter()
        .chain(
            stem.split(['/', '\\', '.', ':'])
                .filter(|segment| !segment.is_empty())
                .map(|segment| segment.to_ascii_lowercase().replace('_', "-"))
                // `crate`, `self`, and `super` say where to start, and the
                // caller already knows: it is the file doing the importing.
                .filter(|segment| !matches!(segment.as_str(), "crate" | "self" | "super")),
        )
        .collect()
}

/// Whether a line is bringing another file in.
fn is_import_line(trimmed: &str) -> bool {
    const OPENERS: &[&str] = &[
        "import ",
        "import(",
        "from ",
        "use ",
        "#include",
        "include ",
        "require ",
        "@import",
        "export ",
        "using ",
        "extern crate",
        "source ",
        "load(",
        "mod ",
        "pub use ",
        "pub mod ",
    ];
    const ANYWHERE: &[&str] = &["require(", "import(", "importlib", "from importlib"];
    OPENERS.iter().any(|opener| trimmed.starts_with(opener))
        || ANYWHERE.iter().any(|mark| trimmed.contains(mark))
}

fn analyze_file(absolute: &Path, relative: &Path) -> Result<(Node, Vec<Vec<String>>)> {
    let metadata = fs::metadata(absolute).map_err(|error| {
        whichever(
            error,
            format!("could not read metadata for {}", absolute.display()),
        )
    })?;
    let name = relative
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| relative.display().to_string());
    let extension = relative
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let category = classify(relative, &extension);

    let mut metrics = Metrics {
        bytes: metadata.len(),
        file_count: 1,
        ..Metrics::default()
    };
    let mut imports = Vec::new();
    if metadata.len() <= MAX_ANALYZED_BYTES && is_text_file(&extension, &name) {
        let content = fs::read(absolute)
            .map_err(|error| whichever(error, format!("could not read {}", absolute.display())))?;
        let text = String::from_utf8_lossy(&content);
        metrics.lines = text.lines().count();
        metrics.code_lines = text
            .lines()
            .filter(|line| {
                let line = line.trim();
                !line.is_empty()
                    && !line.starts_with("//")
                    && !line.starts_with('#')
                    && !line.starts_with("<!--")
            })
            .count();
        metrics.complexity = estimate_complexity(&text, category);
        imports = imported_paths(&text);
    }

    Ok((
        Node {
            name,
            relative_path: relative.to_path_buf(),
            kind: NodeKind::File { category },
            metrics,
            children: Vec::new(),
        },
        imports,
    ))
}

fn sort_and_aggregate(node: &mut Node) -> Metrics {
    if !node.is_directory() {
        return node.metrics;
    }

    node.children.sort_by(|left, right| {
        right
            .is_directory()
            .cmp(&left.is_directory())
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
    });
    let mut metrics = Metrics::default();
    for child in &mut node.children {
        metrics.add(sort_and_aggregate(child));
    }
    node.metrics = metrics;
    metrics
}

fn classify(path: &Path, extension: &str) -> Category {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let components: Vec<String> = path
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_ascii_lowercase())
        .collect();
    let is_test_path = components.iter().any(|name| {
        matches!(
            name.as_str(),
            "test" | "tests" | "spec" | "specs" | "__tests__"
        )
    });

    if is_test_path
        || file_name.contains(".test.")
        || file_name.contains(".spec.")
        || file_name.starts_with("test_")
    {
        return Category::Test;
    }

    let is_script_path = components
        .iter()
        .any(|name| matches!(name.as_str(), "script" | "scripts" | "bin" | "tools"));
    if is_script_path
        && matches!(
            extension,
            "js" | "ts" | "py" | "rb" | "sh" | "bash" | "zsh" | "fish" | "ps1" | "bat"
        )
    {
        return Category::Script;
    }

    let is_web_path = components.iter().any(|name| {
        matches!(
            name.as_str(),
            "web"
                | "webapp"
                | "webinterface"
                | "frontend"
                | "client"
                | "ui"
                | "components"
                | "views"
                | "public"
                | "styles"
        )
    });

    match extension {
        "rs" | "go" | "c" | "cc" | "cpp" | "cxx" | "h" | "hpp" | "cs" | "java" | "kt" | "swift"
        | "rb" | "php" | "py" | "ex" | "exs" | "scala" | "fs" | "fsx" => Category::Source,
        "js" | "ts" if is_web_path => Category::Web,
        "js" | "ts" => Category::Source,
        "jsx" | "tsx" | "vue" | "svelte" | "html" | "css" | "scss" | "sass" | "less" => {
            Category::Web
        }
        "sh" | "bash" | "zsh" | "fish" | "ps1" | "bat" => Category::Script,
        "md" | "mdx" | "rst" | "txt" | "adoc" => Category::Docs,
        "json" | "jsonc" | "toml" | "yaml" | "yml" | "xml" | "ini" | "conf" | "config"
        | "properties" | "lock" => Category::Config,
        "sql" | "csv" | "tsv" | "graphql" | "proto" => Category::Data,
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "ico" | "bmp" | "woff" | "woff2"
        | "ttf" | "mp3" | "wav" | "mp4" | "webm" => Category::Asset,
        _ if matches!(
            file_name.as_str(),
            "dockerfile" | "makefile" | "justfile" | "procfile"
        ) =>
        {
            Category::Config
        }
        _ => Category::Other,
    }
}

fn is_text_file(extension: &str, name: &str) -> bool {
    !matches!(
        extension,
        "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "webp"
            | "ico"
            | "bmp"
            | "woff"
            | "woff2"
            | "ttf"
            | "zip"
            | "gz"
            | "tar"
            | "pdf"
            | "mp3"
            | "wav"
            | "mp4"
            | "webm"
            | "exe"
            | "dll"
            | "so"
            | "dylib"
    ) && name != "Cargo.lock"
}

fn estimate_complexity(text: &str, category: Category) -> usize {
    if !matches!(
        category,
        Category::Source | Category::Web | Category::Script | Category::Test
    ) {
        return 0;
    }

    const TOKENS: &[&str] = &[
        " if ", " for ", " while ", " match ", " switch ", " catch ", " case ", "&&", "||",
        " else ",
    ];
    text.lines()
        .map(|line| {
            let padded = format!(" {} ", line.trim());
            TOKENS
                .iter()
                .map(|token| padded.matches(token).count())
                .sum::<usize>()
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    fn find<'a>(node: &'a Node, name: &str) -> &'a Node {
        if node.name == name {
            return node;
        }
        node.children
            .iter()
            .map(|child| find(child, name))
            .find(|found| found.name == name)
            .unwrap_or(node)
    }

    #[test]
    fn a_file_counts_the_other_files_that_import_it() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        fs::create_dir(root.join("src")).unwrap();
        fs::write(root.join("src/engine.rs"), "pub fn run() {}\n").unwrap();
        fs::write(root.join("src/quiet.rs"), "pub fn idle() {}\n").unwrap();
        for index in 0..3 {
            fs::write(
                root.join(format!("src/caller_{index}.rs")),
                "use crate::engine;\nfn go() { engine::run(); }\n",
            )
            .unwrap();
        }
        // Naming a file outside an import line is not a reference to it.
        fs::write(
            root.join("src/prose.rs"),
            "// the engine is described here, and quiet is too\n",
        )
        .unwrap();

        let scanned = scan_repository(root, &ScanOptions::default()).unwrap();
        assert_eq!(find(&scanned.root, "engine.rs").metrics.references, 3);
        assert_eq!(find(&scanned.root, "quiet.rs").metrics.references, 0);
        // A folder carries what its files are owed.
        assert_eq!(find(&scanned.root, "src").metrics.references, 3);
    }

    #[test]
    fn importing_the_same_file_twice_still_counts_once() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        fs::write(root.join("engine.rs"), "pub fn run() {}\n").unwrap();
        fs::write(
            root.join("caller.rs"),
            "use crate::engine::run;\nuse crate::engine::stop;\nuse crate::engine;\n",
        )
        .unwrap();

        let scanned = scan_repository(root, &ScanOptions::default()).unwrap();
        assert_eq!(find(&scanned.root, "engine.rs").metrics.references, 1);
    }

    #[test]
    fn a_barrel_file_is_referred_to_by_its_folder() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        fs::create_dir_all(root.join("widgets")).unwrap();
        fs::write(root.join("widgets/mod.rs"), "pub mod inner;\n").unwrap();
        fs::write(root.join("app.rs"), "use crate::widgets;\n").unwrap();

        let scanned = scan_repository(root, &ScanOptions::default()).unwrap();
        assert_eq!(find(&scanned.root, "mod.rs").metrics.references, 1);
    }

    /// The case the old word-matching reading could not see at all. A crate
    /// root is imported by the crate's name, which appears nowhere in the
    /// file's own path spelling — `repo_city_map` against
    /// `crates/repo-city-map/src/lib.rs` — so the file every other crate
    /// depends on scored zero.
    #[test]
    fn a_crate_root_is_referred_to_by_the_crate_name() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        fs::create_dir_all(root.join("crates/repo-city-map/src")).unwrap();
        fs::create_dir_all(root.join("crates/viewer/src")).unwrap();
        fs::write(
            root.join("crates/repo-city-map/src/lib.rs"),
            "pub struct MapBuilding;\n",
        )
        .unwrap();
        for name in ["one", "two", "three"] {
            fs::write(
                root.join(format!("crates/viewer/src/{name}.rs")),
                "use repo_city_map::MapBuilding;\n",
            )
            .unwrap();
        }

        let scanned = scan_repository(root, &ScanOptions::default()).unwrap();
        assert_eq!(find(&scanned.root, "lib.rs").metrics.references, 3);
    }

    /// A relative import says exactly which file it means, so two files with
    /// the same name in different folders must not both be credited.
    #[test]
    fn a_relative_import_picks_one_of_two_files_sharing_a_name() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        fs::create_dir_all(root.join("web/util")).unwrap();
        fs::create_dir_all(root.join("server/util")).unwrap();
        fs::write(root.join("web/util/format.js"), "export const f = 1;\n").unwrap();
        fs::write(root.join("server/util/format.js"), "export const f = 2;\n").unwrap();
        fs::write(
            root.join("web/page.js"),
            "import { f } from './util/format';\n",
        )
        .unwrap();

        let scanned = scan_repository(root, &ScanOptions::default()).unwrap();
        let web = find(&scanned.root, "web");
        let server = find(&scanned.root, "server");
        assert_eq!(web.metrics.references, 1, "the imported file was not found");
        assert_eq!(
            server.metrics.references, 0,
            "a relative import credited a file in another folder"
        );
    }

    /// Only the target of an import is a reference. The item taken from it is
    /// not a file, and matching it against one is how a heuristic invents
    /// dependencies that were never written.
    #[test]
    fn the_item_taken_from_a_module_is_not_itself_a_reference() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        fs::create_dir(root.join("src")).unwrap();
        fs::write(root.join("src/engine.rs"), "pub struct Renderer;\n").unwrap();
        // A file named after the *item* the import takes, not after the path.
        fs::write(root.join("src/renderer.rs"), "pub fn unrelated() {}\n").unwrap();
        fs::write(
            root.join("src/app.rs"),
            "use crate::engine::Renderer;\nfn go(_: Renderer) {}\n",
        )
        .unwrap();

        let scanned = scan_repository(root, &ScanOptions::default()).unwrap();
        assert_eq!(find(&scanned.root, "engine.rs").metrics.references, 1);
        assert_eq!(
            find(&scanned.root, "renderer.rs").metrics.references,
            0,
            "the item taken from a module was matched against a file name"
        );
    }

    /// A deeper path is the more specific claim, so it wins over a bare name
    /// that would have matched several files.
    #[test]
    fn a_longer_path_beats_a_bare_name() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        fs::create_dir_all(root.join("app/models")).unwrap();
        fs::create_dir_all(root.join("app/views")).unwrap();
        fs::write(root.join("app/models/user.py"), "class User: pass\n").unwrap();
        fs::write(root.join("app/views/user.py"), "def show(): pass\n").unwrap();
        fs::write(
            root.join("app/main.py"),
            "from app.models.user import User\n",
        )
        .unwrap();

        let scanned = scan_repository(root, &ScanOptions::default()).unwrap();
        let models = find(&scanned.root, "models");
        let views = find(&scanned.root, "views");
        assert_eq!(models.metrics.references, 1);
        assert_eq!(views.metrics.references, 0, "the vaguer match was credited");
    }

    /// Imports of things outside the repository resolve to nothing rather than
    /// to whatever file happens to share a word with them.
    #[test]
    fn an_import_from_outside_the_repository_credits_nothing() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        fs::create_dir(root.join("src")).unwrap();
        fs::write(root.join("src/collections.rs"), "pub fn mine() {}\n").unwrap();
        fs::write(
            root.join("src/app.rs"),
            "use std::collections::HashMap;\nuse serde::Serialize;\n",
        )
        .unwrap();

        let scanned = scan_repository(root, &ScanOptions::default()).unwrap();
        assert_eq!(
            find(&scanned.root, "collections.rs").metrics.references,
            0,
            "a standard library path was matched against a file in the repository"
        );
    }

    /// A C-style include names a file with its extension attached.
    #[test]
    fn a_quoted_include_resolves_to_the_header_it_names() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        fs::create_dir_all(root.join("include/gfx")).unwrap();
        fs::write(root.join("include/gfx/engine.h"), "void run(void);\n").unwrap();
        fs::write(
            root.join("main.c"),
            "#include <stdio.h>\n#include \"include/gfx/engine.h\"\n",
        )
        .unwrap();

        let scanned = scan_repository(root, &ScanOptions::default()).unwrap();
        assert_eq!(find(&scanned.root, "engine.h").metrics.references, 1);
    }

    #[test]
    fn scans_hierarchy_and_aggregates_metrics() {
        let directory = tempdir().unwrap();
        fs::create_dir(directory.path().join("src")).unwrap();
        fs::create_dir(directory.path().join("tests")).unwrap();
        fs::write(
            directory.path().join("src/main.rs"),
            "fn main() {\n if true { println!(\"hi\"); }\n}\n",
        )
        .unwrap();
        fs::write(
            directory.path().join("tests/app.rs"),
            "#[test]\nfn works() {}\n",
        )
        .unwrap();

        let repository = scan_repository(
            directory.path(),
            &ScanOptions {
                max_files: 10,
                include_ignored: false,
            },
        )
        .unwrap();

        assert_eq!(repository.root.metrics.file_count, 2);
        assert_eq!(repository.root.metrics.lines, 5);
        assert_eq!(repository.root.children.len(), 2);
        let test_file = &repository.root.children[1].children[0];
        assert!(matches!(
            test_file.kind,
            NodeKind::File {
                category: Category::Test
            }
        ));
    }

    #[test]
    fn reports_files_above_limit() {
        let directory = tempdir().unwrap();
        for index in 0..3 {
            fs::write(directory.path().join(format!("{index}.rs")), "fn f() {}").unwrap();
        }
        let repository = scan_repository(
            directory.path(),
            &ScanOptions {
                max_files: 2,
                include_ignored: false,
            },
        )
        .unwrap();

        assert_eq!(repository.root.metrics.file_count, 2);
        assert_eq!(repository.omitted_files, 1);
    }

    #[test]
    fn distinguishes_backend_web_and_automation_code() {
        assert_eq!(
            classify(Path::new("controllers/task.js"), "js"),
            Category::Source
        );
        assert_eq!(
            classify(Path::new("webinterface/src/store.js"), "js"),
            Category::Web
        );
        assert_eq!(
            classify(Path::new("scripts/dev.js"), "js"),
            Category::Script
        );
    }

    #[test]
    fn discovers_repositories_without_descending_into_them() {
        let directory = tempdir().unwrap();
        let first = directory.path().join("first");
        let second = directory.path().join("group/second");
        fs::create_dir_all(first.join(".git")).unwrap();
        fs::create_dir_all(first.join("nested/.git")).unwrap();
        fs::create_dir_all(second.join(".git")).unwrap();

        let repositories = discover_repositories(directory.path(), 3).unwrap();

        assert_eq!(repositories, vec![first, second]);
    }
}
