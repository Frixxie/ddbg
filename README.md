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

## Install

Prebuilt binaries are published on
[GitHub Releases](https://github.com/Frixxie/ddbg/releases) for Linux
(x86_64, aarch64), macOS (x86_64, Apple Silicon) and Windows (x86_64).

Preferred, prebuilt binary via [cargo-binstall](https://github.com/cargo-bins/cargo-binstall):

```console
cargo binstall ddbg
```

Compile locally from crates.io:

```console
cargo install ddbg --locked
```

Installer script, macOS/Linux:

```console
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/Frixxie/ddbg/releases/latest/download/ddbg-installer.sh | sh
```

Installer script, Windows (PowerShell):

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://github.com/Frixxie/ddbg/releases/latest/download/ddbg-installer.ps1 | iex"
```

Manual: download the archive for your platform from
[GitHub Releases](https://github.com/Frixxie/ddbg/releases), check it against
the `.sha256` file, extract it and put `ddbg` on your `PATH`.

## Releasing

Releases are built by [dist](https://github.com/axodotdev/cargo-dist), which
generates `.github/workflows/release.yml`. Don't edit that file by hand; change
`[workspace.metadata.dist]` in `Cargo.toml` and run `dist generate`.

Versions and [CHANGELOG.md](CHANGELOG.md) are managed with
[git-cliff](https://git-cliff.org) (config in `cliff.toml`). Use
[Conventional Commits](https://www.conventionalcommits.org) (`feat:`, `fix:`,
`feat!:` …) so git-cliff can pick the next version: before 1.0, `feat` and
breaking changes bump the minor version, everything else bumps the patch.

```console
scripts/release.sh          # or: scripts/release.sh 0.2.0 to force a version
```

This bumps the workspace version, regenerates `CHANGELOG.md`, runs the tests
and commits `chore(release): vX.Y.Z`. It then prints the remaining steps:
publish to crates.io, push, and push the `vX.Y.Z` tag.

Pushing the tag builds all targets and creates the GitHub Release with
archives, checksums and installers. dist uses the matching `CHANGELOG.md`
section as the release notes.

## Build from source

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
| `--attach <pid>` | Attach immediately to an existing local process |
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
| `attach <pid>` | | Attach to an existing local process using the session's adapter |
| `detach` | | Disconnect and leave the process running |
| `continue` | `c` | Resume execution |
| `pause` | | Interrupt the program (also Ctrl-C) |
| `kill` | `k` | Terminate the program |
| `next` | `n` | Step over |
| `step` | `s` | Step into |
| `finish` | `fin` | Step out |
| `break <file>:<line>` | `b` | Set a breakpoint at a line |
| `break [<file>:]<function>` | `b` | Set a function breakpoint, optionally only in `<file>` |
| `break <location> if <expr>` | `b` | Set a source or function breakpoint with a condition |
| `condition <id> [expr]` | | Change a breakpoint condition; omit the expression to clear it |
| `delete <id>` | `d` | Delete a breakpoint |
| `breakpoints` | | List breakpoints |
| `backtrace` | `bt` | Show the call stack |
| `threads` / `thread <id>` | | List / select threads |
| `frame [n]`, `up`, `down` | `f` | Select or show a stack frame |
| `print <expr>` | `p` | Evaluate an expression |
| `watch <expr>` | | Keep an expression and refresh it at each stop |
| `watches` | | List watch expressions and their current values/errors |
| `unwatch <id>` | | Remove a watch expression |
| `eval <expr>` | `e` | Evaluate in the adapter's REPL context (may have side effects, e.g. debugger console commands) |
| `set [var] <lvalue> = <value>` | | Assign a value, e.g. `set point.x = 42` |
| `locals` | | Show local variables |
| `help [command]` | `h` | Show help |
| `quit` | `q` | Exit (also Ctrl-D) |

An empty line repeats the last `next`/`step`/`finish`/`continue`. History is
saved between sessions. Press Tab for context-aware completion (aliases work too):

- `break`: file paths and source function names, including `src/file.rs:function`.
  Functions are scanned on first use, using the same discovery as the TUI picker.
- `run`: file paths.
- `test-run`, `test-debug`, `tests`: names from the most recently listed tests.
  `test-debug` completes `-b` / `--break`.
- `print`, `eval`, `set`, `watch`, and breakpoint conditions: expression
  completion from the debug adapter (`completions` request) in the selected
  frame, falling back to local
  variable names when the adapter does not support it.
- `delete`: existing breakpoint IDs; `thread`: thread IDs; `frame`: stack indices.
- `unwatch`: existing watch IDs.
- Command names and `help` topics.

Conditional breakpoints use the debug adapter's expression syntax:

```console
ddbg> break src/main.rs:42 if count > 10
ddbg> break process_item if item.id == 7
ddbg> condition 1 count > 20
ddbg> condition 1
```

Conditions can be configured before launching. If the adapter does not
advertise conditional breakpoint support, ddbg rejects them (including at
launch) rather than silently setting unconditional breakpoints. Repeating
`break` at an existing location preserves its condition; use `condition` to
change or clear it. Conditions persist across program restarts within the
session and appear in the REPL and TUI breakpoint lists. Tab completes
breakpoint IDs for `condition` and expressions after `if` or the ID.
For function breakpoints, prefer qualified names (e.g. `hello_rust::add`)
when a short name could also match other functions. Invalid expressions and
condition evaluation errors are reported according to the adapter's behavior.

### Attaching to a process

```console
ddbg --attach 12345
ddbg --adapter "netcoredbg --interpreter=vscode" --attach 12345
# Or from the REPL/TUI command line:
ddbg> attach 12345
ddbg> pause
ddbg> backtrace
ddbg> detach
```

Attach is local and PID-based. The adapter is selected from the current project,
or defaults to LLDB; a PID alone does not identify the language. Use `--adapter`
when the process does not match the current project. `--attach` cannot be combined
with a program, `--run`, or `--stop-on-entry`.

Breakpoints and watch expressions survive detach and reattach. Attachment does
not guarantee an immediate stop; use `pause` if the process continues running.
`run` does not restart an attached process: use `attach <pid>` to reattach or
`run <program>` to switch to launching.

**Quitting, ending an MCP session, or replacing an attached session detaches
without killing the external process.** `kill` explicitly requests termination
and fails if the adapter cannot terminate an attached process. Detaching a
launched process requires the adapter's termination-override capability.

OS debugging permissions still apply (e.g. Linux ptrace restrictions and macOS
debugging authorization). Python PID attach uses debugpy injection and is
experimental; it may require additional platform tooling and permissions.
Remote/socket attach is not supported. Adapter startup is bounded to 30 seconds;
control commands can queue behind the startup handshake until it completes or
times out. Driver/MCP halt-wait timeouts apply after startup completes.

The driver exposes `Debugger::attach(pid)` and `Debugger::detach()`. MCP exposes
`attach { pid, timeout_ms? }` and `detach` after `start_session`; `detach` remains
usable while another tool is waiting for the process to halt.

### Watch expressions

```console
ddbg> watch point.x
Watch 1: point.x = <unavailable>
ddbg> run
# At each stop, after the source listing:
Watch 1: point.x = 3
ddbg> watches
Watch 1: point.x = 3
ddbg> unwatch 1
Deleted watch 1
```

Watches use the adapter's expression syntax and the selected frame. They
refresh at every stop, frame/thread selection, value assignment, and `eval`.
Expressions persist across program restarts within the session; current values
are cleared when execution resumes or ends. An out-of-scope or invalid
expression shows an error for that watch without hiding the debugger stop.
`watches` lists cached summaries without evaluating the expressions again.
Use `print <expression>` to inspect children on demand.

These are **watch expressions, not watchpoints**: they do not interrupt
execution when memory changes. Avoid expressions with side effects because
they are evaluated automatically. Watch IDs are separate from breakpoint IDs.

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
Run and debug select the discovered display name, including a `[Theory]` row's
arguments, rather than running all of the method's rows. Display names must be
unique; names beginning or ending in `*` are rejected to avoid wildcard selection. Other
MTP frameworks are listed but cannot be run or debugged individually yet.

netcoredbg launch arguments are encoded separately to preserve literal quotes
in string-valued theory rows. Unsupported argument forms fail explicitly rather
than being silently dropped: whitespace plus a trailing backslash on Unix,
and empty arguments or certain unquoted trailing-backslash forms on Windows.

The driver exposes `debug_test_result()` and MCP resume/wait/output responses
include `test_result` for a debug-test launch. This reports runner-derived
`outcome` and execution `counts` separately from the adapter's `exit_code`.
Zero tests or more than one selected test are reported as failed selection;
missing/incomplete summaries and interrupted tests are `unknown`, not passed.
Results survive output reads and reset on a new launch. Manual `run`/`attach`
operations do not inherit a prior debug-test result. MCP output strips ANSI
terminal formatting; raw driver output is preserved.

Debugger exit codes are reported by the adapter, not independently verified by
ddbg. Some netcoredbg/runtime combinations have reported zero for a non-zero
.NET process exit. Use the ordinary test runner to verify pass/fail results;
a debugger-reported zero alone is not proof of success.

MCP `start_session(stop_on_entry: true)` queues startup asynchronously. Call
`wait` to observe the entry stop before inspecting frames or resuming.

`ddbg --version` includes the source revision and build timestamp; MCP
`start_session` returns these as `build_revision` and `build_timestamp` alongside
`version`. These identify a build, not a cryptographic binary fingerprint.
Source archives can set `DDBG_BUILD_REVISION` at build time;
`SOURCE_DATE_EPOCH` supplies the timestamp for reproducible builds.

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
- A **watches** pane below locals appears when expressions are configured.
  Use `:watch <expr>`, `:watches`, and `:unwatch <id>` to manage it.
- **Output pane:** the event and command log.
- **Bottom line:** key hints, or the `:` command line when it is open.

Press `?` to list the keys. Press `:` to enter any REPL command, such as
`:break src/main.rs:14` or `:print point`. Up/Down browse command history,
which is not saved between sessions. Tab completes like the REPL; with
several matches a list opens and Tab / Shift-Tab cycle through it.

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
| `=` | Edit the selected variable's value (locals pane) |
| `.` | Jump to the execution point |
| Tab | Cycle focus: source, stack, locals, output |
| `j`/`k`, Up/Down, PageUp/PageDown, `g`/`G` | Move / scroll |
| `:` | Command line |
| `?` | Help |
| `q` | Quit |

In the locals pane, select a variable with Up/Down or `j`/`k` and press
Enter to view its name, type, and full value supplied by the debugger.
Long values wrap in the popup; use Up/Down, PageUp/PageDown, or `g`/`G`
to scroll. Press Esc or Enter to close it. Press `=` to edit the selected
variable: the bottom line shows `set <name> = <value>`; Enter assigns the new
value and Esc cancels.

`set` uses the adapter's `setExpression` request when available, otherwise
`setVariable`; with `setVariable`, members (`a.b`, `a->b`, `a[2]`) are
resolved through their parent, so `set point.x = 1` works with `lldb-dap`.

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

## MCP server

`ddbg mcp` serves the debugger to AI agents over the
[Model Context Protocol](https://modelcontextprotocol.io) on stdin/stdout.
Example client configuration:

```json
{ "mcpServers": { "ddbg": { "command": "ddbg", "args": ["mcp"] } } }
```

One debug session is active at a time. Call `start_session` with the project
directory (detection works as for the CLI), then use `set_breakpoint`, `run`,
`continue`, `next`, `step`, `finish`, `pause`, `wait`, `kill`, `backtrace`,
`threads`, `select_thread`, `select_frame`, `evaluate`, `set_value`, `locals`,
`list_tests`, `run_test`, `debug_test`, `get_output` and `end_session`. `repl`
runs any REPL command line. Resuming tools wait until the program stops,
exits or terminates; after `timeout_ms` (default 30 s) they report `running`.
While a call waits, `pause`, `kill` and `end_session` act immediately. Results
contain the REPL's text rendering plus structured JSON.
Logging works as in the CLI (`--log-dap`, `DDBG_LOG`) and never writes to
stdout.

`set_breakpoint` accepts an optional `condition`; `set_breakpoint_condition`
changes an existing condition by ID, or clears it when `condition` is omitted.
Breakpoint results include the condition. The programmatic driver exposes
`Debugger::break_if(location, expression)` and `Debugger::condition(id, condition)`.

`add_watch` takes an `expression`, `list_watches` returns current summaries,
and `remove_watch` takes an `id`. Stopped results include watch summaries and
per-expression errors. The driver exposes `Debugger::watch`, `watches`, and
`unwatch`, and `StopInfo::watches` contains the values at that stop.

`wait` returns the current stopped or terminal state even if its event was
already consumed by another tool. Launching or resuming clears that state.
If `pause` is requested before the adapter exposes a live thread, ddbg retries
thread discovery until it can interrupt or the program stops/exits. A tool
timeout limits the wait, not the lifetime of that pending pause request.

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

The driver's .NET regression also requires SDK 10 and `netcoredbg` on PATH:

```console
cargo test -p ddbg-driver --test netcoredbg --locked -- --ignored
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
