//! Function discovery shared with the REPL.

pub use ddbg_cli::functions::{Function, discover};

impl crate::picker::PickerItem for Function {
    fn label(&self) -> &str {
        &self.label
    }
}
