use std::path::PathBuf;

use ddbg_dap::protocol as dap;

/// Adapter frame id. Only valid while stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FrameId(pub i64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackFrame {
    pub id: FrameId,
    pub name: String,
    /// Local path of the source, if the adapter reported one.
    pub path: Option<PathBuf>,
    /// Display name of the source (file name).
    pub source_name: Option<String>,
    pub line: u32,
    pub column: u32,
}

impl From<dap::StackFrame> for StackFrame {
    fn from(f: dap::StackFrame) -> Self {
        let (path, source_name) = match f.source {
            Some(s) => (s.path.map(PathBuf::from), s.name),
            None => (None, None),
        };
        Self {
            id: FrameId(f.id),
            name: f.name,
            path,
            source_name,
            line: f.line.max(0) as u32,
            column: f.column.max(0) as u32,
        }
    }
}
