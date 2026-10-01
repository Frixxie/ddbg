//! Debug engine and frontend-independent session state.

pub mod adapter;
pub mod breakpoint;
pub mod command;
pub mod error;
pub mod event;
pub mod frame;
pub mod session;
pub mod target;
pub mod thread;
pub mod variable;

pub use command::{Command, Reply};
pub use error::{Error, Result};
pub use event::DebugEvent;
pub use session::{DebugSession, SessionStatus};
pub use target::{DebugTarget, LaunchTarget};
