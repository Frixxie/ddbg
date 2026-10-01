use std::fmt;

use ddbg_dap::protocol as dap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ThreadId(pub i64);

impl fmt::Display for ThreadId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thread {
    pub id: ThreadId,
    pub name: String,
}

impl From<dap::Thread> for Thread {
    fn from(t: dap::Thread) -> Self {
        Self {
            id: ThreadId(t.id),
            name: t.name,
        }
    }
}
