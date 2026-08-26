//! What a scan found, before any of it is given a place to stand.
//!
//! This is the shape of the codebase itself: a tree of files and folders with
//! a few counted facts attached. Nothing here knows about islands, wards, or
//! geometry — [`layout`](crate::build::layout) is what turns it into a settlement.

use std::path::PathBuf;

/// The counted facts about a file, or the sum of them for a folder.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Metrics {
    /// Size on disk.
    pub bytes: u64,
    /// Total lines, blank and commented ones included.
    pub lines: usize,
    /// Lines that are neither blank nor an obvious comment.
    pub code_lines: usize,
    /// A rough branch count, used for how ornate a building looks rather than
    /// as a claim about the code.
    pub complexity: usize,
    /// Files at or below this node. A file counts one.
    pub file_count: usize,
    /// How many other files refer to this one, by import or include.
    ///
    /// This is the only fact here that is about a file's place in the
    /// codebase rather than about the file itself, which is what makes it
    /// worth counting: it is the closest thing a scan can get to how much the
    /// rest of the repository leans on this file.
    pub references: usize,
}

impl Metrics {
    /// Folds a child's totals into this one.
    pub fn add(&mut self, other: Metrics) {
        self.bytes += other.bytes;
        self.lines += other.lines;
        self.code_lines += other.code_lines;
        self.complexity += other.complexity;
        self.file_count += other.file_count;
        self.references += other.references;
    }
}

/// What a file is for.
///
/// The category is what fixes a file's architecture and colour, so it stays
/// stable across repositories: a test looks like a watchtower everywhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Category {
    /// Source code and business logic.
    Source,
    /// Web and user interface code.
    Web,
    /// Tests and verification.
    Test,
    /// Documentation and project knowledge.
    Docs,
    /// Configuration, manifests, and project rules.
    Config,
    /// Data, schemas, queries, and protocols.
    Data,
    /// Images, fonts, audio, and other binary assets.
    Asset,
    /// Scripts, automation, and developer tooling.
    Script,
    /// Anything that fits nowhere else.
    Other,
}

/// Whether a node is a folder or a file.
#[derive(Clone, Debug)]
pub enum NodeKind {
    /// A folder, which becomes a ward or a neighbourhood.
    Directory,
    /// A file, which becomes a holding.
    File {
        /// What the file is for.
        category: Category,
    },
}

/// One file or folder in the scanned tree.
#[derive(Clone, Debug)]
pub struct Node {
    /// The final component of the path, which is what a label shows.
    pub name: String,
    /// Where this sits relative to the repository root.
    pub relative_path: PathBuf,
    /// Whether this is a folder or a file.
    pub kind: NodeKind,
    /// This node's own facts, or the sum of its children's.
    pub metrics: Metrics,
    /// Contained nodes. Always empty for a file.
    pub children: Vec<Node>,
}

impl Node {
    /// An empty folder node.
    pub fn directory(name: String, relative_path: PathBuf) -> Self {
        Self {
            name,
            relative_path,
            kind: NodeKind::Directory,
            metrics: Metrics::default(),
            children: Vec::new(),
        }
    }

    /// Whether this node is a folder.
    pub fn is_directory(&self) -> bool {
        matches!(self.kind, NodeKind::Directory)
    }
}

/// A scanned repository.
#[derive(Debug)]
pub struct Repository {
    /// The root folder, with the whole tree hanging off it.
    pub root: Node,
    /// Where the repository was read from.
    pub source_path: PathBuf,
    /// How many files the scan left out to stay within
    /// [`ScanOptions::max_files`](crate::build::scan::ScanOptions::max_files).
    ///
    /// Worth surfacing: a town missing half its holdings should say so rather
    /// than quietly under-reporting the codebase.
    pub omitted_files: usize,
}
