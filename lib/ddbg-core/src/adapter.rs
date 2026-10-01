//! Adapter-specific integration: how to start an adapter and how to turn a
//! generic [`DebugTarget`](crate::target::DebugTarget) into its launch/attach
//! arguments. Intentionally small.

use std::path::Path;

use ddbg_dap::AdapterCommand;
use ddbg_dap::transport::find_program;
use serde_json::{Value, json};

use crate::target::{AttachTarget, LaunchTarget};
use anyhow::{Result, anyhow};

pub trait DebugAdapter: Send + Sync {
    /// Value for `initialize.adapterID`.
    fn id(&self) -> &str;

    /// Resolved adapter command.
    fn command(&self) -> Result<AdapterCommand>;

    fn build_launch_request(&self, target: &LaunchTarget) -> Result<Value>;

    fn build_attach_request(&self, target: &AttachTarget) -> Result<Value>;
}

fn resolve(program: &str, args: &[String]) -> Result<AdapterCommand> {
    let path = find_program(program).ok_or_else(|| anyhow!("{program} was not found in PATH"))?;
    Ok(AdapterCommand {
        program: path.to_string_lossy().into_owned(),
        args: args.to_vec(),
    })
}

fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// Pick an adapter integration for a user-supplied command.
pub fn adapter_for_command(program: &str, args: Vec<String>) -> Box<dyn DebugAdapter> {
    let name = Path::new(program)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if name.starts_with("netcoredbg") {
        let default = NetCoreDbgAdapter::default();
        Box::new(NetCoreDbgAdapter {
            program: program.to_owned(),
            args: if args.is_empty() { default.args } else { args },
        })
    } else {
        Box::new(LldbDapAdapter {
            program: program.to_owned(),
            args,
            init_commands: rust_init_commands(),
        })
    }
}

// ---------------------------------------------------------------------------
// lldb-dap
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct LldbDapAdapter {
    pub program: String,
    pub args: Vec<String>,
    /// LLDB commands run before the target is created.
    pub init_commands: Vec<String>,
}

impl Default for LldbDapAdapter {
    fn default() -> Self {
        Self {
            program: "lldb-dap".into(),
            args: Vec::new(),
            init_commands: Vec::new(),
        }
    }
}

impl LldbDapAdapter {
    /// lldb-dap with the Rust toolchain's LLDB pretty-printers loaded.
    pub fn for_rust() -> Self {
        Self {
            init_commands: rust_init_commands(),
            ..Self::default()
        }
    }
}

/// Commands that load rustc's LLDB formatters (what `rust-lldb` does).
pub fn rust_init_commands() -> Vec<String> {
    let Ok(out) = std::process::Command::new("rustc")
        .args(["--print", "sysroot"])
        .output()
    else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    let sysroot = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    let etc = Path::new(&sysroot).join("lib/rustlib/etc");
    let lookup = etc.join("lldb_lookup.py");
    let commands = etc.join("lldb_commands");
    if !lookup.is_file() {
        return Vec::new();
    }
    // Newer toolchains register everything from `__lldb_init_module`; older
    // ones additionally ship an `lldb_commands` file.
    let mut init = vec![format!("command script import \"{}\"", lookup.display())];
    if commands.is_file() {
        init.push(format!("command source -s 0 \"{}\"", commands.display()));
    }
    init
}

impl DebugAdapter for LldbDapAdapter {
    fn id(&self) -> &str {
        "lldb-dap"
    }

    fn command(&self) -> Result<AdapterCommand> {
        resolve(&self.program, &self.args)
    }

    fn build_launch_request(&self, t: &LaunchTarget) -> Result<Value> {
        let env: Vec<String> = t.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
        let mut v = json!({
            "name": "ddbg",
            "type": "lldb-dap",
            "request": "launch",
            "program": path_str(&t.absolute_program()),
            "args": t.args,
            "cwd": path_str(&t.cwd),
            "env": env,
            "stopOnEntry": t.stop_on_entry,
        });
        if !self.init_commands.is_empty() {
            v["initCommands"] = json!(self.init_commands);
        }
        Ok(v)
    }

    fn build_attach_request(&self, t: &AttachTarget) -> Result<Value> {
        let mut v = json!({
            "name": "ddbg",
            "type": "lldb-dap",
            "request": "attach",
            "pid": t.pid,
        });
        if !self.init_commands.is_empty() {
            v["initCommands"] = json!(self.init_commands);
        }
        Ok(v)
    }
}

// ---------------------------------------------------------------------------
// netcoredbg
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct NetCoreDbgAdapter {
    pub program: String,
    pub args: Vec<String>,
}

impl Default for NetCoreDbgAdapter {
    fn default() -> Self {
        Self {
            program: "netcoredbg".into(),
            args: vec!["--interpreter=vscode".into()],
        }
    }
}

impl DebugAdapter for NetCoreDbgAdapter {
    fn id(&self) -> &str {
        "coreclr"
    }

    fn command(&self) -> Result<AdapterCommand> {
        resolve(&self.program, &self.args)
    }

    fn build_launch_request(&self, t: &LaunchTarget) -> Result<Value> {
        Ok(json!({
            "name": "ddbg",
            "type": "coreclr",
            "request": "launch",
            "program": path_str(&t.absolute_program()),
            "args": t.args,
            "cwd": path_str(&t.cwd),
            "env": t.env,
            "stopAtEntry": t.stop_on_entry,
        }))
    }

    fn build_attach_request(&self, t: &AttachTarget) -> Result<Value> {
        Ok(json!({
            "name": "ddbg",
            "type": "coreclr",
            "request": "attach",
            "processId": t.pid,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> LaunchTarget {
        let mut t = LaunchTarget::new("bin/app", vec!["--x".into()]);
        t.cwd = "/proj".into();
        t.env.insert("RUST_LOG".into(), "debug".into());
        t
    }

    #[test]
    fn lldb_launch_arguments() {
        let v = LldbDapAdapter::default()
            .build_launch_request(&target())
            .unwrap();
        assert_eq!(v["program"], "/proj/bin/app");
        assert_eq!(v["args"], json!(["--x"]));
        assert_eq!(v["env"], json!(["RUST_LOG=debug"]));
        assert_eq!(v["stopOnEntry"], false);
        assert!(v.get("initCommands").is_none());
    }

    #[test]
    fn netcoredbg_launch_arguments() {
        let v = NetCoreDbgAdapter::default()
            .build_launch_request(&target())
            .unwrap();
        assert_eq!(v["type"], "coreclr");
        assert_eq!(v["env"], json!({"RUST_LOG": "debug"}));
        assert_eq!(v["stopAtEntry"], false);
    }

    #[test]
    fn picks_adapter_by_command_name() {
        assert_eq!(
            adapter_for_command("/usr/bin/netcoredbg", vec![]).id(),
            "coreclr"
        );
        assert_eq!(adapter_for_command("lldb-dap", vec![]).id(), "lldb-dap");
    }

    #[test]
    fn missing_adapter_is_reported() {
        let a = LldbDapAdapter {
            program: "definitely-not-a-real-adapter".into(),
            ..Default::default()
        };
        let err = a.command().unwrap_err();
        assert_eq!(
            err.to_string(),
            "definitely-not-a-real-adapter was not found in PATH"
        );
    }
}
