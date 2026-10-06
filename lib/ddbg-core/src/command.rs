//! Frontend-independent command model. Aliases live only in frontends.

use crate::breakpoint::{Breakpoint, BreakpointId};
pub use crate::breakpoint::{FunctionLocation, Location};
use crate::frame::StackFrame;
use crate::target::{AttachTarget, LaunchTarget};
use crate::thread::{Thread, ThreadId};
use crate::variable::{Evaluation, VarRef, Variable};
use crate::watch::{Watch, WatchId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Launch the given target, or the last one when `None`.
    Run(Option<LaunchTarget>),
    /// Attach to an existing local process.
    Attach(AttachTarget),
    /// Disconnect and leave the debuggee running.
    Detach,
    Continue,
    Pause,
    Kill,

    Next,
    Step,
    Finish,

    Break(Location),
    /// Set a breakpoint with an adapter-native condition.
    ConditionalBreak {
        location: Location,
        condition: String,
    },
    /// Change a breakpoint's condition, or clear it with `None`.
    Condition {
        id: BreakpointId,
        condition: Option<String>,
    },
    DeleteBreakpoint(BreakpointId),
    Breakpoints,

    Backtrace,
    Threads,
    Thread(ThreadId),
    Frame(FrameSelector),

    Print(String),
    /// Keep an expression and evaluate it in the selected frame at each stop.
    Watch(String),
    Unwatch(WatchId),
    Watches,
    /// Evaluate in the adapter's REPL context (may have side effects).
    Eval(String),
    /// Assign `value` to the l-value expression `target`.
    Set {
        target: String,
        value: String,
    },
    /// Assign `value` to the variable `name` inside container `scope`.
    SetVariable {
        scope: VarRef,
        name: String,
        value: String,
    },
    /// Complete an expression; `column` is a 0-based char offset in `text`.
    Complete {
        text: String,
        column: usize,
    },
    Locals,

    Tests(TestQuery),
    TestRun(TestSelector),
    /// Debug a test; with `break_at_start`, first set a breakpoint at the
    /// start of the test.
    TestDebug {
        test: TestSelector,
        break_at_start: bool,
    },

    Quit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameSelector {
    /// Show the current frame.
    Current,
    Index(usize),
    Up,
    Down,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TestQuery {
    pub filter: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestSelector {
    /// Index into the most recently displayed test list (1-based).
    Index(usize),
    Name(String),
}

/// Structured result of a command, rendered by the frontend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    Ok,
    /// The program was launched under the debugger.
    Launched(std::path::PathBuf),
    Attached(u32),
    BreakpointSet {
        breakpoint: Breakpoint,
        new: bool,
    },
    BreakpointDeleted(BreakpointId),
    Breakpoints(Vec<Breakpoint>),
    Backtrace {
        frames: Vec<StackFrame>,
        selected: Option<usize>,
    },
    Threads {
        threads: Vec<Thread>,
        selected: Option<ThreadId>,
    },
    Frame {
        index: usize,
        frame: StackFrame,
    },
    /// An evaluated expression and (one level of) its children.
    Value(Evaluation, Vec<Variable>),
    WatchSet {
        watch: Watch,
        new: bool,
    },
    WatchDeleted(WatchId),
    Watches(Vec<Watch>),
    Locals(Vec<ScopeVariables>),
    Completions(Vec<Completion>),
    Quit,
}

/// An expression completion. `start`/`length` are char offsets into the
/// completed text describing what to replace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    pub label: String,
    pub text: String,
    pub kind: Option<String>,
    pub start: usize,
    pub length: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeVariables {
    pub scope: String,
    pub reference: VarRef,
    pub variables: Vec<Variable>,
}
