//! Desired breakpoint configuration.
//!
//! ddbg owns the *desired* breakpoints; the adapter owns the actual ones.
//! DAP `setBreakpoints` replaces all breakpoints of a source, so the store
//! is organized to hand out the full set per file. Likewise,
//! `setFunctionBreakpoints` replaces all function breakpoints at once.
//!
//! DAP function breakpoints are name-only. A function breakpoint may carry
//! a `file` scope; ddbg enforces it itself by skipping stops whose top frame
//! lies outside that file (see [`Breakpoint::in_scope`]).

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

/// A function breakpoint, optionally restricted to one source file.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FunctionLocation {
    pub name: String,
    /// File scope as written by the user, matched as a path suffix.
    pub file: Option<PathBuf>,
}

impl FunctionLocation {
    pub fn new(name: impl Into<String>, file: Option<PathBuf>) -> Self {
        Self {
            name: name.into(),
            file,
        }
    }
}

impl fmt::Display for FunctionLocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.file {
            Some(file) => write!(f, "{}:{}", file.display(), self.name),
            None => f.write_str(&self.name),
        }
    }
}

/// Where the user asked to stop.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Location {
    Source(SourceLocation),
    Function(FunctionLocation),
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Location::Source(s) => s.fmt(f),
            Location::Function(func) => func.fmt(f),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Breakpoint {
    pub id: BreakpointId,
    pub requested: Location,
    pub resolved: Option<SourceLocation>,
    pub verified: bool,
    pub message: Option<String>,
    /// Adapter-assigned id, used to match `breakpoint` events.
    pub adapter_id: Option<i64>,
}

impl Breakpoint {
    pub fn is_function(&self) -> bool {
        matches!(self.requested, Location::Function(_))
    }

    /// The file scope of a scoped function breakpoint.
    pub fn scope(&self) -> Option<&Path> {
        match &self.requested {
            Location::Function(f) => f.file.as_deref(),
            Location::Source(_) => None,
        }
    }

    /// Whether a stop at `path` satisfies this breakpoint's file scope.
    /// Unscoped breakpoints accept any location.
    pub fn in_scope(&self, path: Option<&Path>) -> bool {
        match self.scope() {
            None => true,
            Some(scope) => path.is_some_and(|p| path_matches(p, scope)),
        }
    }
}

/// `scope` matches `path` if it is a component-wise suffix of it,
/// so `main.rs` and `src/main.rs` both match `/proj/src/main.rs`.
pub fn path_matches(path: &Path, scope: &Path) -> bool {
    path.ends_with(scope)
}

#[derive(Debug, Default, Clone)]
pub struct BreakpointStore {
    next_id: u32,
    items: BTreeMap<BreakpointId, Breakpoint>,
}

impl BreakpointStore {
    /// Add a breakpoint. Relative paths are resolved against `base`.
    /// Returns the existing id if an identical breakpoint already exists.
    /// Function breakpoint file scopes are kept as written (suffix match).
    pub fn add(&mut self, mut location: Location, base: &Path) -> (BreakpointId, bool) {
        if let Location::Source(src) = &mut location {
            src.path = normalize(&src.path, base);
        }
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
            .filter_map(|b| match &b.requested {
                Location::Source(s) => Some(s.path.clone()),
                Location::Function(_) => None,
            })
            .collect();
        files.sort();
        files.dedup();
        files
    }

    /// Breakpoints of one file, in the order sent to the adapter.
    pub fn for_file(&self, path: &Path) -> Vec<&Breakpoint> {
        self.items
            .values()
            .filter(|b| matches!(&b.requested, Location::Source(s) if s.path == path))
            .collect()
    }

    /// All function breakpoints, in the order sent to the adapter.
    pub fn functions(&self) -> Vec<&Breakpoint> {
        self.items.values().filter(|b| b.is_function()).collect()
    }

    pub fn has_functions(&self) -> bool {
        self.items.values().any(|b| b.is_function())
    }

    /// Build the complete `setFunctionBreakpoints` request.
    pub fn function_request(&self) -> dap::SetFunctionBreakpointsArguments {
        dap::SetFunctionBreakpointsArguments {
            breakpoints: self
                .functions()
                .into_iter()
                .filter_map(|b| match &b.requested {
                    Location::Function(f) => Some(dap::FunctionBreakpoint {
                        name: f.name.clone(),
                        condition: None,
                    }),
                    Location::Source(_) => None,
                })
                .collect(),
        }
    }

    /// Apply a `setFunctionBreakpoints` response. Results are positional.
    pub fn apply_function_results(&mut self, results: &[dap::Breakpoint]) {
        let ids: Vec<_> = self.functions().iter().map(|b| b.id).collect();
        for (id, result) in ids.into_iter().zip(results) {
            if let Some(bp) = self.items.get_mut(&id) {
                update(bp, result);
            }
        }
    }

    /// Mark all function breakpoints as unsupported by the adapter.
    pub fn reject_functions(&mut self, message: &str) {
        for bp in self.items.values_mut().filter(|b| b.is_function()) {
            bp.verified = false;
            bp.resolved = None;
            bp.adapter_id = None;
            bp.message = Some(message.to_owned());
        }
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
                .filter_map(|b| match &b.requested {
                    Location::Source(s) => Some(dap::SourceBreakpoint {
                        line: s.line.into(),
                        column: None,
                        condition: None,
                    }),
                    Location::Function(_) => None,
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

    /// A breakpoint stop proves the breakpoint is bound, even if the adapter
    /// never sent a `breakpoint` event (netcoredbg binds lazily on module
    /// load). Marks `hit` verified, or, when the adapter reported no ids,
    /// the unverified source breakpoints at `at`. Returns changed ids.
    pub fn mark_hit(
        &mut self,
        hit: &[BreakpointId],
        at: Option<&SourceLocation>,
    ) -> Vec<BreakpointId> {
        let mut changed = Vec::new();
        for bp in self.items.values_mut() {
            let matches = if hit.is_empty() {
                matches!((&bp.requested, at), (Location::Source(s), Some(at)) if s == at)
            } else {
                hit.contains(&bp.id)
            };
            if matches && !bp.verified {
                bp.verified = true;
                bp.message = None;
                if bp.resolved.is_none() {
                    bp.resolved = at.cloned();
                }
                changed.push(bp.id);
            }
        }
        changed
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
    let path = result
        .source
        .as_ref()
        .and_then(|s| s.path.as_ref())
        .map(PathBuf::from)
        .or_else(|| match &bp.requested {
            Location::Source(s) => Some(s.path.clone()),
            Location::Function(_) => None,
        });
    if let (Some(line), Some(path)) = (result.line, path) {
        bp.resolved = Some(SourceLocation::new(path, line.max(0) as u32));
    }
    if let (Some(scope), Some(resolved)) = (bp.scope(), &bp.resolved)
        && !path_matches(&resolved.path, scope)
        && bp.message.is_none()
    {
        bp.message = Some(format!(
            "resolved outside {}; stops elsewhere are skipped",
            scope.display()
        ));
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

    fn src(path: &str, line: u32) -> Location {
        Location::Source(SourceLocation::new(path, line))
    }

    fn func(name: &str, file: Option<&str>) -> Location {
        Location::Function(FunctionLocation::new(name, file.map(PathBuf::from)))
    }

    fn resolved(id: i64, path: &str, line: i64) -> dap::Breakpoint {
        dap::Breakpoint {
            id: Some(id),
            verified: true,
            message: None,
            source: Some(dap::Source {
                name: None,
                path: Some(path.into()),
                source_reference: None,
            }),
            line: Some(line),
            column: None,
        }
    }

    #[test]
    fn function_breakpoints_are_sent_together_and_not_per_file() {
        let mut s = store();
        s.add(func("main", None), Path::new("/"));
        s.add(func("parse", Some("src/b.rs")), Path::new("/"));
        let names: Vec<_> = s
            .function_request()
            .breakpoints
            .into_iter()
            .map(|b| b.name)
            .collect();
        assert_eq!(names, ["main", "parse"]);
        assert_eq!(s.files().len(), 2);
        assert_eq!(s.request_for(Path::new("/src/a.rs")).breakpoints.len(), 2);
        // scope is kept as written, and duplicates are detected
        assert!(!s.add(func("parse", Some("src/b.rs")), Path::new("/x")).1);
    }

    #[test]
    fn scoped_function_breakpoints_check_paths() {
        let mut s = BreakpointStore::default();
        let (scoped, _) = s.add(func("parse", Some("src/b.rs")), Path::new("/"));
        let (free, _) = s.add(func("main", None), Path::new("/"));
        s.apply_function_results(&[resolved(7, "/p/src/a.rs", 3), resolved(8, "/p/m.rs", 1)]);

        let bp = s.get(scoped).unwrap();
        assert!(bp.in_scope(Some(Path::new("/p/src/b.rs"))));
        assert!(!bp.in_scope(Some(Path::new("/p/src/a.rs"))));
        assert!(!bp.in_scope(Some(Path::new("/p/xsrc/b.rs"))));
        assert!(!bp.in_scope(None));
        assert!(bp.message.as_deref().unwrap().contains("outside src/b.rs"));

        let bp = s.get(free).unwrap();
        assert!(bp.in_scope(None));
        assert_eq!(bp.message, None);
        assert_eq!(bp.resolved, Some(SourceLocation::new("/p/m.rs", 1)));
    }

    fn store() -> BreakpointStore {
        let mut s = BreakpointStore::default();
        s.add(src("/src/a.rs", 10), Path::new("/"));
        s.add(src("/src/b.rs", 5), Path::new("/"));
        s.add(src("/src/a.rs", 20), Path::new("/"));
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
        let (id, new) = s.add(src("/src/a.rs", 10), Path::new("/"));
        assert_eq!(id, BreakpointId(1));
        assert!(!new);
    }

    #[test]
    fn relative_paths_are_resolved() {
        let mut s = BreakpointStore::default();
        let (id, _) = s.add(src("src/x.rs", 1), Path::new("/proj"));
        assert_eq!(s.get(id).unwrap().requested, src("/proj/src/x.rs", 1));
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
