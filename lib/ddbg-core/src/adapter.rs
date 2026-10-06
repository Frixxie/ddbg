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
    } else if name.starts_with("python") || name.starts_with("debugpy") {
        let default = DebugpyAdapter::default();
        Box::new(DebugpyAdapter {
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
        let args = t
            .args
            .iter()
            .map(|arg| netcoredbg_argument(arg))
            .collect::<Result<Vec<_>>>()?;
        Ok(json!({
            "name": "ddbg",
            "type": "coreclr",
            "request": "launch",
            "program": path_str(&t.absolute_program()),
            "args": args,
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

/// netcoredbg (including 3.2.0) wraps each DAP argument in double quotes
/// without escaping its contents before calling dbgshim.CreateProcessForLaunch.
/// Encode literal quotes here, not in LaunchTarget: other adapters and direct
/// process execution expect the original argv. Unix dbgshim removes only the
/// backslash immediately before a quote; it does not use Windows argv rules.
#[cfg(not(windows))]
fn netcoredbg_argument(arg: &str) -> Result<String> {
    if arg.ends_with('\\') && arg.chars().any(char::is_whitespace) {
        return Err(anyhow!(
            "netcoredbg cannot preserve an argument with whitespace and a trailing backslash: {arg:?}"
        ));
    }
    // netcoredbg skips empty DAP arguments. A pair of syntactic quotes is
    // nonempty to the adapter but becomes one empty argument in Unix dbgshim.
    Ok(if arg.is_empty() {
        "\"\"".into()
    } else {
        arg.replace('"', "\\\"")
    })
}

#[cfg(windows)]
fn netcoredbg_argument(arg: &str) -> Result<String> {
    // Do not silently lose argv entries that netcoredbg cannot represent.
    if arg.is_empty()
        || (arg.ends_with('\\')
            && !arg.contains(' ')
            && (arg.contains('"') || arg.chars().any(char::is_whitespace)))
    {
        return Err(anyhow!("netcoredbg cannot preserve argument {arg:?}"));
    }
    let mut escaped = String::new();
    let mut slashes = 0;
    for c in arg.chars() {
        if c == '\\' {
            slashes += 1;
            continue;
        }
        escaped.extend(std::iter::repeat_n(
            '\\',
            if c == '"' { slashes * 2 + 1 } else { slashes },
        ));
        escaped.push(c);
        slashes = 0;
    }
    // netcoredbg adds one extra backslash itself when it quotes a trailing
    // backslash argument containing spaces. Supply the remaining ones.
    let trailing = if slashes > 0 && arg.contains(' ') {
        slashes * 2 - 1
    } else {
        slashes
    };
    escaped.extend(std::iter::repeat_n('\\', trailing));
    Ok(escaped)
}

// ---------------------------------------------------------------------------
// debugpy
// ---------------------------------------------------------------------------

/// Python via debugpy's DAP adapter (`python -m debugpy.adapter`, stdio).
///
/// `program` is the Python interpreter. It must have `debugpy` installed and
/// is also used to run the debuggee.
#[derive(Debug, Clone)]
pub struct DebugpyAdapter {
    pub program: String,
    pub args: Vec<String>,
}

impl Default for DebugpyAdapter {
    fn default() -> Self {
        Self::with_python("python3")
    }
}

impl DebugpyAdapter {
    pub fn with_python(python: impl Into<String>) -> Self {
        Self {
            program: python.into(),
            args: vec!["-m".into(), "debugpy.adapter".into()],
        }
    }

    /// The interpreter, when the adapter is started through one (`python -m`).
    fn python(&self) -> Option<String> {
        let name = Path::new(&self.program).file_name()?.to_string_lossy();
        name.starts_with("python")
            .then(|| find_program(&self.program).map(|p| path_str(&p)))
            .flatten()
    }
}

impl DebugAdapter for DebugpyAdapter {
    fn id(&self) -> &str {
        "debugpy"
    }

    fn command(&self) -> Result<AdapterCommand> {
        let cmd = resolve(&self.program, &self.args)?;
        if self.args.first().is_some_and(|a| a == "-m") {
            let ok = std::process::Command::new(&cmd.program)
                .args(["-c", "import debugpy"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
            if !ok {
                return Err(anyhow!(
                    "debugpy is not installed for {} (try `{} -m pip install debugpy`)",
                    cmd.program,
                    self.program
                ));
            }
        }
        Ok(cmd)
    }

    fn build_launch_request(&self, t: &LaunchTarget) -> Result<Value> {
        let mut v = json!({
            "name": "ddbg",
            "type": "debugpy",
            "request": "launch",
            "args": t.args,
            "cwd": path_str(&t.cwd),
            "env": t.env,
            "stopOnEntry": t.stop_on_entry,
            "console": "internalConsole",
            "justMyCode": true,
            "redirectOutput": true,
        });
        // `-m pkg.module` runs a module, anything else is a script.
        match t.program.to_str() {
            Some("-m") => {
                let (module, rest) = t
                    .args
                    .split_first()
                    .ok_or_else(|| anyhow!("-m needs a module name"))?;
                v["module"] = json!(module);
                v["args"] = json!(rest);
            }
            _ => v["program"] = json!(path_str(&t.absolute_program())),
        }
        if let Some(python) = self.python() {
            v["python"] = json!(python);
        }
        Ok(v)
    }

    fn build_attach_request(&self, t: &AttachTarget) -> Result<Value> {
        Ok(json!({
            "name": "ddbg",
            "type": "debugpy",
            "request": "attach",
            "processId": t.pid,
            "justMyCode": true,
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
    fn netcoredbg_preserves_literal_quotes_without_changing_other_adapters() {
        let mut t = target();
        t.args = vec![r#"Ns.C.M(input: "a \"quoted\" string", path: "C:\folder\file")"#.into()];
        let net = NetCoreDbgAdapter::default()
            .build_launch_request(&t)
            .unwrap();
        #[cfg(not(windows))]
        assert_eq!(net["args"][0], t.args[0].replace('"', "\\\""));
        #[cfg(windows)]
        assert_eq!(net["args"][0], netcoredbg_argument(&t.args[0]).unwrap());
        let lldb = LldbDapAdapter::default().build_launch_request(&t).unwrap();
        assert_eq!(lldb["args"], json!(t.args));
        let python = DebugpyAdapter::default().build_launch_request(&t).unwrap();
        assert_eq!(python["args"], json!(t.args));
    }

    #[cfg(not(windows))]
    #[test]
    fn netcoredbg_empty_and_backslash_arguments() {
        assert_eq!(netcoredbg_argument("").unwrap(), "\"\"");
        assert_eq!(netcoredbg_argument(r"C:\folder\").unwrap(), r"C:\folder\");
        assert!(netcoredbg_argument("folder with spaces\\").is_err());
        assert!(netcoredbg_argument("folder\twith tabs\\").is_err());
    }

    #[cfg(windows)]
    #[test]
    fn netcoredbg_windows_escaping_matches_quoted_argv_rules() {
        assert_eq!(netcoredbg_argument(r#"a\"b"#).unwrap(), r#"a\\\"b"#);
        assert_eq!(netcoredbg_argument(r"C:\folder\").unwrap(), r"C:\folder\");
        // netcoredbg adds the final escape backslash when it quotes this.
        assert_eq!(
            netcoredbg_argument(r"C:\some folder\").unwrap(),
            r"C:\some folder\"
        );
        assert_eq!(
            netcoredbg_argument(r"C:\some folder\\").unwrap(),
            r"C:\some folder\\\"
        );
        assert!(netcoredbg_argument("").is_err());
        assert!(netcoredbg_argument("folder\twith tabs\\").is_err());
    }

    #[test]
    fn attach_arguments_use_each_adapters_pid_field() {
        let t = AttachTarget { pid: 42 };
        let lldb = LldbDapAdapter {
            init_commands: vec!["command script import formatters.py".into()],
            ..Default::default()
        }
        .build_attach_request(&t)
        .unwrap();
        assert_eq!(lldb["pid"], 42);
        assert_eq!(lldb["request"], "attach");
        assert_eq!(
            lldb["initCommands"],
            json!(["command script import formatters.py"])
        );
        let net = NetCoreDbgAdapter::default()
            .build_attach_request(&t)
            .unwrap();
        assert_eq!(net["processId"], 42);
        assert_eq!(net["request"], "attach");
        let python = DebugpyAdapter::default().build_attach_request(&t).unwrap();
        assert_eq!(python["processId"], 42);
        assert_eq!(python["request"], "attach");
    }

    #[test]
    fn picks_adapter_by_command_name() {
        assert_eq!(
            adapter_for_command("/usr/bin/netcoredbg", vec![]).id(),
            "coreclr"
        );
        assert_eq!(adapter_for_command("lldb-dap", vec![]).id(), "lldb-dap");
        assert_eq!(adapter_for_command("python3", vec![]).id(), "debugpy");
        assert_eq!(
            adapter_for_command(".venv/bin/python", vec![]).id(),
            "debugpy"
        );
    }

    #[test]
    fn debugpy_launch_arguments() {
        let a = DebugpyAdapter::with_python("definitely-not-python");
        let mut t = target();
        t.program = "main.py".into();
        let v = a.build_launch_request(&t).unwrap();
        assert_eq!(v["type"], "debugpy");
        assert_eq!(v["program"], "/proj/main.py");
        assert_eq!(v["env"], json!({"RUST_LOG": "debug"}));
        assert_eq!(v["stopOnEntry"], false);
        assert!(v.get("module").is_none());

        let mut t = target();
        t.program = "-m".into();
        t.args = vec!["pkg.app".into(), "--x".into()];
        let v = a.build_launch_request(&t).unwrap();
        assert_eq!(v["module"], "pkg.app");
        assert_eq!(v["args"], json!(["--x"]));
        assert!(v.get("program").is_none());
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
