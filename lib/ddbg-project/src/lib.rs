//! Project detection (Rust / .NET). Not implemented yet.

use std::path::PathBuf;

/// Kind of project discovered in a directory tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectKind {
    Rust,
    DotNet,
}

/// A detected project root.
#[derive(Debug, Clone)]
pub struct Project {
    pub kind: ProjectKind,
    pub root: PathBuf,
}
