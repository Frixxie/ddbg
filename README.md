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
| `--tui` | Full-screen terminal UI (experimental, see [Terminal UI](#terminal-ui)) |
| `--stop-on-entry` | Launch and stop at the program entry point |
| `--no-detect` | Disable project and program auto-detection |
| `--adapter "<cmd> [args]"` | Debug adapter command (default chosen from the detected project), e.g. `--adapter "netcoredbg --interpreter=vscode"` |
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

An empty line repeats the last `next`/`step`/`finish`/`continue`. History is
saved between sessions. Press Tab for context-aware completion (aliases work too):

- `break`: file paths and source function names, including `src/file.rs:function`.
  Functions are scanned on first use, using the same discovery as the TUI picker.
- `run`: file paths.
- `test-run`, `test-debug`, `tests`: names from the most recently listed tests;
  run/debug also complete test numbers. `test-debug` completes `-b` / `--break`.
- `print`: local variable names in the selected frame while stopped.
- `delete`: existing breakpoint IDs; `thread`: thread IDs; `frame`: stack indices.
- Command names and `help` topics.

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

## Terminal UI

`ddbg --tui` opens a full-screen interface. It uses the same engine as the
REPL and accepts all the options above:

```console
ddbg --tui
ddbg --tui --stop-on-entry -- target/debug/my-app --some-arg
```

The screen is divided into these areas:

- **Title bar:** program state (`not started`, `running`, `stopped`, `exited`,
  `terminated`), the current program, and `working...` while a command runs.
- **Source pane:** shows breakpoints in the gutter (`●` verified, `○`
  unverified) and marks the execution line with `▶`.
- **stack**, **locals**, and **breakpoints** panes on the right.
- **Output pane:** the event and command log.
- **Bottom line:** key hints, or the `:` command line when it is open.

Press `?` to list the keys. Press `:` to enter any REPL command, such as
`:break src/main.rs:14` or `:print point`. Up/Down browse command history,
which is not saved between sessions.

| Key | Action |
|---|---|
| `r` | Run / restart (opens the program picker if none is chosen) |
| `e` | Pick a program |
| `t` | Pick a test |
| `F` | Pick a function |
| `c`, F5 | Continue |
| `p`, Ctrl-C | Pause |
| `K` | Kill |
| `n`, F10 | Step over |
| `s`, F11 | Step into |
| `f`, Shift-F11 | Step out |
| `b`, F9 | Toggle breakpoint at the cursor line |
| `u` / `d` | Frame up / down |
| Enter | Select the highlighted frame (stack pane) or view a variable's value (locals pane) |
| `.` | Jump to the execution point |
| Tab | Cycle focus: source, stack, locals, output |
| `j`/`k`, Up/Down, PageUp/PageDown, `g`/`G` | Move / scroll |
| `:` | Command line |
| `?` | Help |
| `q` | Quit |

In the locals pane, select a variable with Up/Down or `j`/`k` and press
Enter to view its name, type, and full value supplied by the debugger.
Long values wrap in the popup; use Up/Down, PageUp/PageDown, or `g`/`G`
to scroll. Press Esc or Enter to close it.

### Pickers

Type to filter the list. Each space-separated word must match, and case is
ignored. Use Up/Down or Ctrl-P/Ctrl-N to move and Esc to close.

| Picker | Enter | Other keys |
|---|---|---|
| Tests (`t`) | Debug the test | Ctrl-R run, Ctrl-B debug with a breakpoint at the test start |
| Programs (`e`) | Run it and make it current | Ctrl-B run and stop on entry |
| Functions (`F`) | Toggle a function breakpoint (the picker stays open) | Ctrl-O open the declaration in the source pane |

The function picker scans source files in the project (Rust, C#, Python,
C/C++) and skips build and virtualenv directories.

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
cargo test -p ddbg-core -p ddbg-driver -- --ignored
```

### Layout

| Crate | Purpose |
|---|---|
| `lib/ddbg-dap` | DAP framing, protocol types, client, adapter process |
| `lib/ddbg-core` | Debug engine, session state, adapter integrations |
| `lib/ddbg-cli` | REPL frontend (library) |
| `lib/ddbg-tui` | Full-screen terminal UI (ratatui) |
| `lib/ddbg-driver` | Programmatic driver: run REPL commands from code (typed or text), for tests and scripting |
| `bin/ddbg` | The `ddbg` binary |
| `lib/ddbg-project` | Project detection (Cargo, .NET, Python, C/C++) |
