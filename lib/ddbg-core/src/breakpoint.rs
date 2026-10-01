//! Desired breakpoint configuration.
//!
//! ddbg owns the *desired* breakpoints; the adapter owns the actual ones.
//! DAP `setBreakpoints` replaces all breakpoints of a source, so the store
//! is organized to hand out the full set per file.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use ddbg_dap::protocol as dap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BreakpointId(pub u32);

impl fmt::Display for BreakpointId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceLocation {
    pub path: PathBuf,
    pub line: u32,
}

impl SourceLocation {
    pub fn new(path: impl Into<PathBuf>, line: u32) -> Self {
        Self {
            path: path.into(),
            line,
        }
    }
}

impl fmt::Display for SourceLocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.path.display(), self.line)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Breakpoint {
    pub id: BreakpointId,
    pub requested: SourceLocation,
    pub resolved: Option<SourceLocation>,
    pub verified: bool,
    pub message: Option<String>,
    /// Adapter-assigned id, used to match `breakpoint` events.
    pub adapter_id: Option<i64>,
}

#[derive(Debug, Default, Clone)]
pub struct BreakpointStore {
    next_id: u32,
    items: BTreeMap<BreakpointId, Breakpoint>,
}

impl BreakpointStore {
    /// Add a breakpoint. Relative paths are resolved against `base`.
    /// Returns the existing id if an identical breakpoint already exists.
    pub fn add(&mut self, mut location: SourceLocation, base: &Path) -> (BreakpointId, bool) {
        location.path = normalize(&location.path, base);
        if let Some(bp) = self.items.values().find(|b| b.requested == location) {
            return (bp.id, false);
        }
        self.next_id += 1;
        let id = BreakpointId(self.next_id);
        self.items.insert(
            id,
            Breakpoint {
                id,
                requested: location,
                resolved: None,
                verified: false,
                message: None,
                adapter_id: None,
            },
        );
        (id, true)
    }

    pub fn remove(&mut self, id: BreakpointId) -> Option<Breakpoint> {
        self.items.remove(&id)
    }

    pub fn get(&self, id: BreakpointId) -> Option<&Breakpoint> {
        self.items.get(&id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Breakpoint> {
        self.items.values()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// All files that have at least one breakpoint.
    pub fn files(&self) -> Vec<PathBuf> {
        let mut files: Vec<_> = self
            .items
            .values()
            .map(|b| b.requested.path.clone())
            .collect();
        files.sort();
        files.dedup();
        files
    }

    /// Breakpoints of one file, in the order sent to the adapter.
    pub fn for_file(&self, path: &Path) -> Vec<&Breakpoint> {
        self.items
            .values()
            .filter(|b| b.requested.path == path)
            .collect()
    }

    /// Build the complete `setBreakpoints` request for a file.
    pub fn request_for(&self, path: &Path) -> dap::SetBreakpointsArguments {
        dap::SetBreakpointsArguments {
            source: dap::Source {
                name: path.file_name().map(|n| n.to_string_lossy().into_owned()),
                path: Some(path.to_string_lossy().into_owned()),
                source_reference: None,
            },
            breakpoints: self
                .for_file(path)
                .into_iter()
                .map(|b| dap::SourceBreakpoint {
                    line: b.requested.line.into(),
                    column: None,
                    condition: None,
                })
                .collect(),
        }
    }

    /// Apply a `setBreakpoints` response. Results are positional.
    pub fn apply_results(&mut self, path: &Path, results: &[dap::Breakpoint]) {
        let ids: Vec<_> = self.for_file(path).iter().map(|b| b.id).collect();
        for (id, result) in ids.into_iter().zip(results) {
            if let Some(bp) = self.items.get_mut(&id) {
                update(bp, result);
            }
        }
    }

    /// Apply a `breakpoint` event. Returns the affected breakpoint id.
    pub fn apply_event(&mut self, result: &dap::Breakpoint) -> Option<BreakpointId> {
        let adapter_id = result.id?;
        let bp = self
            .items
            .values_mut()
            .find(|b| b.adapter_id == Some(adapter_id))?;
        update(bp, result);
        Some(bp.id)
    }

    /// Map adapter breakpoint ids (from `stopped.hitBreakpointIds`) to ours.
    pub fn by_adapter_id(&self, adapter_id: i64) -> Option<&Breakpoint> {
        self.items
            .values()
            .find(|b| b.adapter_id == Some(adapter_id))
    }

    /// Forget adapter-side state, e.g. when a session ends.
    pub fn reset_resolution(&mut self) {
        for bp in self.items.values_mut() {
            bp.resolved = None;
            bp.verified = false;
            bp.message = None;
            bp.adapter_id = None;
        }
    }
}

fn update(bp: &mut Breakpoint, result: &dap::Breakpoint) {
    bp.verified = result.verified;
    bp.message = result.message.clone();
    if result.id.is_some() {
        bp.adapter_id = result.id;
    }
    if let Some(line) = result.line {
        let path = result
            .source
            .as_ref()
            .and_then(|s| s.path.as_ref())
            .map(PathBuf::from)
            .unwrap_or_else(|| bp.requested.path.clone());
        bp.resolved = Some(SourceLocation::new(path, line.max(0) as u32));
    }
}

fn normalize(path: &Path, base: &Path) -> PathBuf {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    abs.canonicalize().unwrap_or(abs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> BreakpointStore {
        let mut s = BreakpointStore::default();
        s.add(SourceLocation::new("/src/a.rs", 10), Path::new("/"));
        s.add(SourceLocation::new("/src/b.rs", 5), Path::new("/"));
        s.add(SourceLocation::new("/src/a.rs", 20), Path::new("/"));
        s
    }

    #[test]
    fn request_contains_whole_file() {
        let s = store();
        let req = s.request_for(Path::new("/src/a.rs"));
        let lines: Vec<_> = req.breakpoints.iter().map(|b| b.line).collect();
        assert_eq!(lines, [10, 20]);
    }

    #[test]
    fn duplicate_add_returns_existing() {
        let mut s = store();
        let (id, new) = s.add(SourceLocation::new("/src/a.rs", 10), Path::new("/"));
        assert_eq!(id, BreakpointId(1));
        assert!(!new);
    }

    #[test]
    fn relative_paths_are_resolved() {
        let mut s = BreakpointStore::default();
        let (id, _) = s.add(SourceLocation::new("src/x.rs", 1), Path::new("/proj"));
        assert_eq!(
            s.get(id).unwrap().requested.path,
            Path::new("/proj/src/x.rs")
        );
    }

    #[test]
    fn applies_results_positionally_and_events_by_id() {
        let mut s = store();
        let path = Path::new("/src/a.rs");
        s.apply_results(
            path,
            &[
                dap::Breakpoint {
                    id: Some(100),
                    verified: true,
                    message: None,
                    source: None,
                    line: Some(11),
                    column: None,
                },
                dap::Breakpoint {
                    id: Some(101),
                    verified: false,
                    message: Some("pending".into()),
                    source: None,
                    line: None,
                    column: None,
                },
            ],
        );
        let bp1 = s.get(BreakpointId(1)).unwrap();
        assert!(bp1.verified);
        assert_eq!(bp1.resolved.as_ref().unwrap().line, 11);
        assert!(!s.get(BreakpointId(3)).unwrap().verified);

        let id = s.apply_event(&dap::Breakpoint {
            id: Some(101),
            verified: true,
            message: None,
            source: None,
            line: Some(21),
            column: None,
        });
        assert_eq!(id, Some(BreakpointId(3)));
        assert!(s.get(BreakpointId(3)).unwrap().verified);
    }
}
