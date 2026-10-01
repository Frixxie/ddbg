# ddbg

A terminal-first, language-agnostic debugger with first-class test debugging,
built on the Debug Adapter Protocol (DAP). See [DESIGN.md](DESIGN.md).

> Status: early. Program and test debugging work for Rust (`lldb-dap`) and,
> experimentally, .NET xUnit v3 on Microsoft.Testing.Platform (`netcoredbg`).
> Program debugging (no test discovery yet) works for C/C++ (`lldb-dap`) and
> Python (`debugpy`).

## Requirements

- Rust (edition 2024, tested with 1.98)
- A debug adapter:
  - **Rust / C / C++:** `lldb-dap`. On macOS it ships with the Xcode Command
    Line Tools (`xcode-select --install`) and is found via `xcrun`
    automatically. On Linux install LLVM's `lldb-dap` and put it on `PATH`.
  - **.NET (experimental):** `netcoredbg`, passed with `--adapter`.
  - **Python:** `debugpy` (`python3 -m pip install debugpy`). ddbg runs
    `python -m debugpy.adapter` with the project's interpreter: `$VIRTUAL_ENV`,
    else `.venv`/`venv`/`env` in the project root, else `python3` on `PATH`.

The adapter is picked from the detected project (`Cargo.toml`, `*.csproj`,
`pyproject.toml`/`setup.py`/`requirements.txt`, `CMakeLists.txt`/`Makefile`/
`meson.build`) or, for `ddbg -- <program>`, from its extension (`.dll`, `.py`).
Use `ddbg -- -m package.module` to debug a Python module. `--adapter` overrides
the choice, e.g. `--adapter ".venv/bin/python -m debugpy.adapter"`.

## Build

```console
cargo build --release
```

The binary is `target/release/ddbg`. To install it on your `PATH`:

```console
cargo install --path bin/ddbg
```

## Usage

Check that the adapter works:

```console
ddbg adapter-test lldb-dap
```

Debug a program (build it with debug info first):

```console
cargo build
ddbg -- target/debug/my-app --some-arg
```

Or start empty and pick the program from the prompt:

```console
ddbg
ddbg> run target/debug/my-app --some-arg
```

Example session:

```console
ddbg> break src/main.rs:14
Breakpoint 1 at src/main.rs:14
ddbg> run
Starting program: target/debug/my-app
Breakpoint 1, my_app::main at src/main.rs:14

14 >     let total = add(point.x, point.y);
ddbg> print point
{x:3, y:4}
ddbg> next
ddbg> continue
Process exited normally.
```

### Options

| Option | Description |
|---|---|
| `-- <program> [args...]` | Program to debug and its arguments |
| `--run` | Launch immediately instead of waiting for `run` |
| `--tui` | Full-screen terminal UI (experimental; `?` lists keys, `:` takes REPL commands) |
| `--stop-on-entry` | Launch and stop at the program entry point |
| `--adapter "<cmd> [args]"` | Debug adapter command (default `lldb-dap`), e.g. `--adapter "netcoredbg --interpreter=vscode"` |
| `-v`, `--verbose` | Show debug adapter console messages |
| `--log-dap <file>` | Log DAP traffic to a file |

`DDBG_LOG` sets a tracing filter (e.g. `DDBG_LOG=dap=trace`). Without
`--log-dap`, logs go to `ddbg.log` in the current directory.

### Commands

| Command | Alias | Description |
|---|---|---|
| `run [program [args...]]` | `r` | Start (or restart) the program |
| `continue` | `c` | Resume execution |
| `pause` | | Interrupt the program (also Ctrl-C) |
| `kill` | `k` | Terminate the program |
| `next` | `n` | Step over |
| `step` | `s` | Step into |
| `finish` | `fin` | Step out |
| `break <file>:<line>` | `b` | Set a breakpoint at a line |
| `break [<file>:]<function>` | `b` | Set a function breakpoint, optionally only in `<file>` |
| `delete <id>` | `d` | Delete a breakpoint |
| `breakpoints` | | List breakpoints |
| `backtrace` | `bt` | Show the call stack |
| `threads` / `thread <id>` | | List / select threads |
| `frame [n]`, `up`, `down` | `f` | Select or show a stack frame |
| `print <expr>` | `p` | Evaluate an expression |
| `locals` | | Show local variables |
| `help [command]` | `h` | Show help |
| `quit` | `q` | Exit (also Ctrl-D) |

An empty line repeats the last `next`/`step`/`finish`/`continue`. Tab
completes commands and file paths; history is saved between sessions.

### Tests

Inside a Cargo or .NET project, `tests` builds the test executables and lists them;
`test-run` and `test-debug` take a number from that list or a name:

```console
ddbg> tests parser
1  parser::tests::empty_input
2  parser::tests::invalid_header
ddbg> break src/parser.rs:42
ddbg> test-debug 2
```

`test-debug -b <test>` (or `--break`) also sets a breakpoint at the start of the test.

.NET support covers xUnit v3 test projects on Microsoft.Testing.Platform
(`<UseMicrosoftTestingPlatformRunner>true</UseMicrosoftTestingPlatformRunner>`).
Running a single row of a `[Theory]` runs all of that method's rows. Other
MTP frameworks are listed but cannot be run or debugged individually yet.

## Development

```console
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```

End-to-end tests against real adapters (`fixtures/hello-rust`, `hello-c` via
`make`, and `hello-python` with `$DDBG_PYTHON`, default `python3`, which must
have `debugpy`):

```console
cargo test -p ddbg-core -- --ignored
```

### Layout

| Crate | Purpose |
|---|---|
| `lib/ddbg-dap` | DAP framing, protocol types, client, adapter process |
| `lib/ddbg-core` | Debug engine, session state, adapter integrations |
| `lib/ddbg-cli` | REPL frontend (library) |
| `bin/ddbg` | The `ddbg` binary |
| `lib/ddbg-project` | Project detection (Cargo, .NET, Python, C/C++) |
