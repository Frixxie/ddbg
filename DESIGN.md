# ddbg — Terminal-first DAP debugger

**Status:** Draft  
**Target:** v0.1  
**Implementation language:** Rust  
**Working name:** `ddbg`

## 1. Summary

`ddbg` is a terminal-first, language-agnostic debugger frontend built on the Debug Adapter Protocol (DAP).

It provides a GDB-inspired command interface with first-class support for discovering, running, and debugging tests.

The initial release targets:

- Rust
  - Cargo projects and workspaces
  - Rust/libtest tests
  - `lldb-dap`
- .NET
  - projects and solutions
  - Microsoft.Testing.Platform
  - initial xUnit support
  - `netcoredbg`

The primary workflow should be:

```console
$ ddbg

ddbg> tests parser
1  parser::tests::empty_input
2  parser::tests::invalid_header

ddbg> break src/parser.rs:42
Breakpoint 1 set

ddbg> test-debug 2

Breakpoint 1
src/parser.rs:42

42 > let header = parse_header(input)?;

ddbg> print input
"&broken"

ddbg> next
```

The same commands should work in a .NET repository:

```console
$ ddbg

Detected .NET workspace

ddbg> tests CreateUser
1  UserServiceTests.CanCreateUser

ddbg> test-debug 1

Breakpoint 1
UserService.cs:54

ddbg> print request
CreateUserRequest {
    Name = "Fred"
}
```

The frontend should not contain language-specific debugger logic. DAP provides the abstraction between the frontend and debugger adapters. DAP is explicitly designed so one debugger UI can communicate with multiple debugger implementations.

---

# 2. Product principles

## Terminal first

The command-line interface is the canonical interface.

The future TUI is another presentation layer over the same application state and commands rather than a separate debugger implementation.

A user should never need the TUI to access core functionality.

## Tests are first-class debug targets

Most debuggers think primarily in terms of:

```text
launch executable
attach process
```

`ddbg` additionally thinks in terms of:

```text
debug test
```

The tool is responsible for translating that request into the appropriate executable, arguments, environment, and DAP operation.

## Language-independent UX

The following should mean essentially the same thing everywhere:

```console
ddbg> break file:42
ddbg> continue
ddbg> next
ddbg> print foo
ddbg> backtrace
ddbg> tests foo
ddbg> test-debug foo
```

Language-specific behavior belongs behind provider interfaces.

## DAP-native, not debugger-emulating

`ddbg` should expose concepts that DAP can represent reliably.

It should not attempt to emulate every GDB or LLDB command.

GDB influences the interaction model, not the implementation.

## Minimal configuration

Common Rust and .NET repositories should work with:

```console
ddbg
```

without requiring a configuration file.

Configuration exists for overrides rather than normal operation.

---

# 3. Goals

Version 0.1 should support:

- discovering the current project/workspace;
- launching a normal program under a debugger;
- setting source breakpoints;
- continuing, pausing and stepping;
- listing threads;
- viewing stack traces;
- selecting frames;
- viewing local variables;
- evaluating expressions;
- discovering tests;
- running one test;
- debugging one test;
- Rust through Cargo + `lldb-dap`;
- .NET through Microsoft.Testing.Platform + `netcoredbg`;
- asynchronous debugger events;
- adapter capability detection;
- readable debugger output;
- command history and completion.

The architecture must allow later support for:

- Python;
- C/C++;
- alternative Rust test harnesses;
- VSTest;
- NUnit/MSTest;
- multiple simultaneous debug sessions;
- remote debugging;
- a Ratatui frontend.

---

# 4. Non-goals for v0.1

The first version will not attempt to provide:

- memory inspection;
- assembly/disassembly UI;
- CPU register UI;
- reverse debugging;
- core dump analysis;
- advanced exception configuration;
- data breakpoints;
- function breakpoints;
- conditional breakpoints;
- multi-process debugging;
- remote debugging;
- SSH orchestration;
- debugger adapter installation;
- graphical breakpoint management;
- persistent watches;
- inline values;
- a full-screen TUI;
- VS Code `launch.json` compatibility;
- every possible Cargo target;
- every .NET test runner.

These features should remain possible without influencing the initial architecture excessively.

---

# 5. High-level architecture

```text
┌─────────────────────────────────────────────┐
│                  Frontends                  │
│                                             │
│       REPL                     TUI          │
│     reedline               ratatui          │
└───────────────┬─────────────────┬───────────┘
                │                 │
                └────────┬────────┘
                         │
                  Application API
                         │
               ┌─────────▼─────────┐
               │   Debug Engine    │
               │                   │
               │ session state     │
               │ commands          │
               │ breakpoints       │
               │ frames            │
               │ variables         │
               └──────┬──────┬─────┘
                      │      │
              ┌───────▼─┐  ┌─▼───────────────┐
              │   DAP   │  │ Test subsystem │
              │ client  │  │                 │
              └─────┬───┘  └────────┬────────┘
                    │               │
        ┌───────────┼───────┐       │
        │           │       │       │
   lldb-dap    netcoredbg   ...   Cargo / MTP
        │           │
        ▼           ▼
      Rust         .NET
```

The important boundary is between:

```text
application/debugger concepts
```

and:

```text
DAP protocol concepts
```

The command layer should never write DAP JSON directly.

---

# 6. Workspace layout

Suggested Rust workspace:

```text
ddbg/
├── Cargo.toml
│
├── crates/
│   ├── ddbg-dap/
│   │   ├── codec.rs
│   │   ├── client.rs
│   │   ├── transport.rs
│   │   ├── protocol.rs
│   │   └── error.rs
│   │
│   ├── ddbg-core/
│   │   ├── session.rs
│   │   ├── breakpoint.rs
│   │   ├── thread.rs
│   │   ├── frame.rs
│   │   ├── variable.rs
│   │   ├── command.rs
│   │   └── event.rs
│   │
│   ├── ddbg-project/
│   │   ├── detect.rs
│   │   ├── rust.rs
│   │   └── dotnet.rs
│   │
│   ├── ddbg-test/
│   │   ├── provider.rs
│   │   ├── rust/
│   │   └── dotnet/
│   │
│   ├── ddbg-cli/
│   │   ├── repl.rs
│   │   ├── parser.rs
│   │   ├── commands.rs
│   │   └── render.rs
│   │
│   └── ddbg/
│       └── main.rs      # `ddbg` binary
```

The root `Cargo.toml` is a virtual workspace manifest. All crates except
`ddbg` are libraries; `ddbg` is a thin binary that calls `ddbg_cli::run()`.

A TUI can later become:

```text
crates/ddbg-tui
```

without changing the debugger core.

---

# 7. Core domain model

The application should maintain one central state object.

```rust
pub struct DebugSession {
    pub status: SessionStatus,
    pub target: Option<DebugTarget>,
    pub capabilities: Capabilities,

    pub breakpoints: BreakpointStore,

    pub threads: Vec<Thread>,
    pub selected_thread: Option<ThreadId>,

    pub stack: Vec<StackFrame>,
    pub selected_frame: Option<FrameId>,

    pub scopes: Vec<Scope>,
}
```

Session status:

```rust
pub enum SessionStatus {
    Disconnected,
    Initializing,
    Configuring,
    Running,
    Stopped(StopReason),
    Terminated,
}
```

This state should be independent of the frontend.

Both:

```text
CLI
```

and eventually:

```text
TUI
```

observe and manipulate the same conceptual state.

---

# 8. DAP layer

## Responsibilities

`ddbg-dap` is responsible only for protocol communication.

It should handle:

- spawning an adapter;
- stdin/stdout communication;
- DAP framing;
- JSON serialization;
- request sequence numbers;
- matching responses with requests;
- asynchronous events;
- adapter-to-client requests;
- adapter shutdown;
- protocol errors.

DAP uses `Content-Length` framed UTF-8 JSON messages over transports such as stdin/stdout. Messages are requests, responses, or events.

Example:

```text
Content-Length: 123\r\n
\r\n
{"seq":1,"type":"request",...}
```

## Client API

Conceptually:

```rust
pub struct DapClient {
    request_tx: mpsc::Sender<OutgoingRequest>,
    event_tx: broadcast::Sender<DapEvent>,
}
```

The public API should not expose request sequence management:

```rust
impl DapClient {
    pub async fn request<R>(
        &self,
        request: R,
    ) -> Result<R::Response>
    where
        R: DapRequest;

    pub fn subscribe(
        &self,
    ) -> broadcast::Receiver<DapEvent>;
}
```

Internally:

```text
request()
   │
   ├── allocate sequence
   ├── create oneshot channel
   ├── store pending[sequence]
   └── write message

adapter stdout
   │
   ▼
decode
   │
   ├── response
   │     └── pending[request_seq].send(response)
   │
   ├── event
   │     └── event_tx.send(event)
   │
   └── request
         └── client request handler
```

## Adapter requests

DAP is mostly client → adapter, but adapters may make requests back to the client.

A notable example is `runInTerminal`.

The architecture must therefore support:

```rust
enum IncomingMessage {
    Response(Response),
    Event(Event),
    Request(AdapterRequest),
}
```

rather than assuming everything received asynchronously is an event.

---

# 9. DAP session lifecycle

DAP session setup should follow the protocol lifecycle.

```text
spawn adapter
      │
      ▼
initialize
      │
      ▼
capabilities
      │
      ▼
launch / attach
      │
      ▼
initialized event
      │
      ▼
setBreakpoints
      │
      ▼
configurationDone
      │
      ▼
program runs
```

DAP uses capability negotiation during initialization rather than protocol-version branching. Unsupported capabilities should be treated as unavailable.

The core should therefore expose checks such as:

```rust
session.supports(Feature::ConditionalBreakpoints)
```

instead of adapter-specific checks:

```rust
if adapter == "lldb-dap" {
    ...
}
```

---

# 10. Stopped-state model

When DAP emits:

```text
stopped
```

`ddbg` should refresh only the minimum required state.

Recommended flow:

```text
stopped
   │
   ▼
threads
   │
   ▼
stackTrace(selected thread)
   │
   ▼
select top frame
   │
   ▼
scopes(selected frame)
```

Variables should be loaded lazily.

DAP variable references are only valid for the current suspended state and become invalid once execution resumes.

Therefore:

```rust
fn on_resume(&mut self) {
    self.scopes.clear();
    self.variable_cache.clear();
    self.stack.clear();
}
```

No variable reference should survive a resume operation.

---

# 11. Debug targets

Everything that can be debugged should eventually reduce to one of two concepts:

```rust
pub enum DebugTarget {
    Launch(LaunchTarget),
    Attach(AttachTarget),
}
```

For example:

```rust
pub struct LaunchTarget {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: HashMap<String, String>,
}
```

and:

```rust
pub struct AttachTarget {
    pub pid: u32,
}
```

Test providers do not debug processes themselves.

Instead, they produce a `DebugTarget`.

This is one of the most important architectural boundaries in the project.

---

# 12. Test subsystem

Tests should be modeled independently from languages and frameworks.

```rust
pub trait TestProvider {
    async fn discover(
        &self,
        query: TestQuery,
    ) -> Result<Vec<TestCase>>;

    async fn run(
        &self,
        tests: &[TestId],
    ) -> Result<TestRunResult>;

    async fn debug_target(
        &self,
        test: &TestId,
    ) -> Result<DebugTarget>;
}
```

A test should have structured identity:

```rust
pub struct TestCase {
    pub id: TestId,
    pub name: String,
    pub display_name: String,

    pub source: Option<PathBuf>,
    pub line: Option<u32>,

    pub suite: Option<String>,

    pub provider: ProviderId,
    pub provider_data: ProviderData,
}
```

`TestId` must not simply be the test's display name.

Providers may require additional identity information.

---

# 13. Rust test provider

The Rust provider should drive Cargo through its supported command-line interfaces rather than inspecting Cargo internals.

Cargo explicitly provides `cargo metadata` and JSON build messages for external tools, and recommends the `cargo_metadata` Rust crate for consuming them.

## Workspace discovery

Use:

```console
cargo metadata --format-version 1
```

to discover:

- workspace root;
- packages;
- targets;
- manifests.

## Building test executables

Use:

```console
cargo test --no-run --message-format=json
```

`--no-run` compiles tests without executing them.

Parse Cargo `compiler-artifact` messages and obtain actual executable paths rather than scanning:

```text
target/debug/deps
```

manually.

Flow:

```text
cargo test --no-run --message-format=json
                     │
                     ▼
              compiler-artifact
                     │
                     ▼
              executable path
```

## Test discovery

For every produced test executable:

```console
/path/to/test-binary --list
```

Parse the libtest output into `TestCase` values.

Caching test discovery may be added later.

## Test execution

A single Rust test should approximately execute as:

```console
test-binary full::test::name --exact
```

When debugging, add:

```console
--nocapture
--test-threads=1
```

unless overridden.

The generated debug target becomes:

```rust
DebugTarget::Launch(LaunchTarget {
    program: test_binary,
    args: vec![
        test.name,
        "--exact",
        "--nocapture",
        "--test-threads=1",
    ],
    ..
})
```

and is passed to `lldb-dap`.

`lldb-dap` exposes LLDB through DAP specifically for debugger frontends such as editors and IDEs.

---

# 14. .NET test provider

Version 0.1 should prioritize Microsoft.Testing.Platform.

MTP embeds the testing platform into the test application, and MTP test projects can be built as directly runnable/debuggable executables.

This is significantly easier to integrate cleanly than starting with VSTest.

## Project discovery

The provider should locate:

```text
*.sln
*.slnx
*.csproj
```

and determine which projects are test projects.

Initial detection may rely on project/package metadata.

Eventually MSBuild evaluation may be preferable.

## MTP integration

MTP now includes a server mode explicitly intended for editor, IDE, and test-tool integrations using JSON-RPC.

There are therefore two possible implementation phases.

### Phase A

Use process execution and command-line filtering.

This minimizes initial implementation complexity.

### Phase B

Adopt MTP server mode for:

- discovery;
- structured test execution;
- live test events;
- richer test metadata.

The test-provider interface should be designed so Phase B does not affect the debugger layer.

## Debugging

Because MTP test projects are directly executable/debuggable, the provider can usually produce:

```rust
DebugTarget::Launch(...)
```

rather than needing to debug `dotnet test` itself.

The debug adapter for .NET v0.1 should be:

```text
netcoredbg
```

Adapter-specific launch arguments must remain within the adapter integration layer.

---

# 15. Project detection

Running:

```console
ddbg
```

should inspect the current directory and walk upward looking for recognizable project roots.

Initial rules:

```text
Cargo.toml
    → Rust

*.sln / *.slnx
*.csproj
    → .NET
```

If both exist:

```console
Multiple project types detected:

1 Rust
2 .NET

Select project:
```

The detection API should be generic:

```rust
pub trait ProjectDetector {
    async fn detect(
        &self,
        path: &Path,
    ) -> Result<Option<Project>>;
}
```

---

# 16. Adapter abstraction

DAP itself does not standardize the arguments supplied to `launch` and `attach`; those are adapter-specific.

Therefore `ddbg` requires a thin adapter integration abstraction:

```rust
pub trait DebugAdapter {
    fn command(&self) -> AdapterCommand;

    fn build_launch_request(
        &self,
        target: &LaunchTarget,
    ) -> Result<serde_json::Value>;

    fn build_attach_request(
        &self,
        target: &AttachTarget,
    ) -> Result<serde_json::Value>;
}
```

Initial implementations:

```text
LldbAdapter
NetCoreDbgAdapter
```

This adapter-specific layer should stay intentionally small.

---

# 17. Command model

Commands should first parse into a frontend-independent AST.

```rust
pub enum Command {
    Run,
    Continue,
    Pause,

    Next,
    Step,
    Finish,

    Break(Location),
    DeleteBreakpoint(BreakpointId),

    Backtrace,
    Threads,
    Frame(FrameSelector),

    Print(String),
    Locals,

    Tests(TestQuery),
    TestRun(TestSelector),
    TestDebug(TestSelector),

    Quit,
}
```

The REPL parser is responsible only for converting text into this representation.

The engine executes it.

This is what later allows the TUI to invoke:

```rust
Command::Continue
```

without pretending to type `"continue"` into the CLI.

---

# 18. CLI commands

Canonical long-form commands:

```text
run
continue
pause

next
step
finish

break <location>
delete <breakpoint>

backtrace
threads
frame <n>

print <expression>
locals

tests [filter]
test-run <test>
test-debug <test>

help
quit
```

Aliases:

```text
r     run
c     continue
n     next
s     step
fin   finish

b     break
bt    backtrace
p     print

tr    test-run
td    test-debug

q     quit
```

Aliases are UI conveniences and should not exist in the core command enum.

---

# 19. Test selection UX

Tests can be selected using:

```console
ddbg> tests parser
1  parser::tests::empty
2  parser::tests::invalid
```

Then:

```console
ddbg> td 2
```

Numeric selections refer to the most recent displayed test set.

Users may also write:

```console
ddbg> td parser::tests::invalid
```

Ambiguous selectors should produce options rather than guessing:

```console
ddbg> td create_user

Multiple tests matched:

1 UserTests.create_user
2 AdminTests.create_user

Use `td <number>`.
```

---

# 20. Breakpoint model

`ddbg` owns the desired breakpoint configuration.

DAP adapters own the actual debugger breakpoints.

```rust
struct Breakpoint {
    id: BreakpointId,
    requested: SourceLocation,
    resolved: Option<SourceLocation>,
    verified: bool,
}
```

DAP `setBreakpoints` replaces the breakpoint set for an entire source rather than incrementally adding one breakpoint.

Therefore:

```console
break foo.rs:10
```

must effectively result in:

```text
load every desired breakpoint for foo.rs
          │
          ▼
setBreakpoints(foo.rs, [...])
```

rather than sending only the newly added breakpoint.

---

# 21. Events

Core application events should be independent of raw DAP events.

For example:

```rust
pub enum DebugEvent {
    SessionStarted,
    SessionStopped(StopInfo),
    SessionContinued,
    SessionTerminated,

    BreakpointsChanged,
    ThreadsChanged,
    FrameChanged,

    Output(Output),
}
```

DAP translation:

```text
DAP stopped
    ↓
DebugEvent::SessionStopped

DAP continued
    ↓
DebugEvent::SessionContinued

DAP output
    ↓
DebugEvent::Output
```

The CLI and TUI consume these events.

---

# 22. Concurrency model

Tokio should run the application.

Primary tasks:

```text
┌─────────────────┐
│ REPL input task │
└────────┬────────┘
         │
         ▼
     command channel
         │
         ▼
┌─────────────────┐
│ debugger engine │
└────────┬────────┘
         │
         ▼
┌─────────────────────┐
│ DAP client           │
│                     │
│ reader task          │
│ writer task          │
│ pending requests     │
└─────────────────────┘
```

The core session state should have one logical owner.

Avoid spreading:

```rust
Arc<Mutex<DebugSession>>
```

through every component if possible.

Prefer message passing:

```text
frontend
   │
   ▼
engine task
   │
   ▼
mutates session
```

This makes state transitions deterministic and greatly simplifies future TUI integration.

---

# 23. Error model

Error messages should identify their domain.

```rust
// Messages are grouped by domain, e.g.
// "lldb-dap was not found in PATH"            (adapter)
// "threads failed: ..."                       (dap)
// "the program is not being run"              (command)
```

All crates use `anyhow` for errors. Errors are reported to the user, not
matched on, so typed error enums add little value at this stage.

Where code later needs to react to a specific failure, introduce a small
marker type and check it with `anyhow::Error::downcast_ref` rather than
reintroducing per-domain error enums.

Errors shown in the REPL should be concise:

```text
error: lldb-dap was not found in PATH
```

Optional verbose/debug logging should contain implementation details.

---

# 24. Logging

Use:

```text
tracing
tracing-subscriber
```

DAP traffic should optionally be logged.

Example:

```console
ddbg --log-dap
```

or:

```text
DDBG_LOG=dap=trace
```

DAP logs should go to a file rather than corrupting the interactive terminal.

This is important because debugging a debugger will otherwise become unpleasant.

---

# 25. Configuration

Configuration should be optional.

Suggested location:

```text
.ddbg.toml
```

Example:

```toml
[adapter.rust]
command = "lldb-dap"

[adapter.dotnet]
command = "netcoredbg"
args = ["--interpreter=vscode"]

[debug]
stop_on_entry = false

[rust.tests]
threads = 1
nocapture = true
```

Precedence:

```text
CLI option
    >
project .ddbg.toml
    >
user configuration
    >
built-in defaults
```

Do not require configuration for standard projects.

---

# 26. REPL technology

Use:

```text
reedline
```

for:

- command history;
- editing;
- completion;
- reverse history search;
- asynchronous output;
- Ctrl-C handling.

Potential completions:

```console
ddbg> br<TAB>
break

ddbg> break src/<TAB>

ddbg> td parser::<TAB>
```

Completion should be implemented as providers so test completion can eventually use test discovery.

---

# 27. TUI direction

The TUI is explicitly post-v0.1.

Likely stack:

```text
ratatui
crossterm
```

Proposed layout:

```text
┌ Source ───────────────────────────┬ Stack ────────────────┐
│ 40 │                              │ > Parser::parse       │
│ 41 │ let x = foo();               │   parser_test         │
│ 42>│ parse(x);                    │   libtest             │
│ 43 │                              │                       │
├ Variables ────────────────────────┼ Tests ────────────────┤
│ input = "&bad"                    │ ✓ parses_empty        │
│ x     = 42                        │ ● invalid_header      │
│ foo   = Foo { ... }               │ ✓ unicode             │
├───────────────────────────────────┴───────────────────────┤
│ ddbg> p foo.name                                          │
│ "hello"                                                   │
└───────────────────────────────────────────────────────────┘
```

The command prompt remains available inside the TUI.

Keyboard commands simply dispatch regular application commands:

```text
F5
    → Command::Continue

F10
    → Command::Next
```

---

# 28. Dependency shortlist

Initial likely dependencies:

```toml
tokio
serde
serde_json
anyhow
tracing
tracing-subscriber

clap
reedline

cargo_metadata

toml
directories
```

Later:

```toml
ratatui
crossterm
```

Avoid large framework dependencies until their value is demonstrated.

---

# 29. Testing strategy

The project itself should be heavily testable without spawning real debuggers.

## DAP codec tests

Input:

```text
Content-Length: ...
```

Verify:

```text
correct decoded message
```

Include:

- fragmented reads;
- multiple messages in one buffer;
- malformed headers;
- malformed JSON;
- Unicode byte lengths.

## DAP client tests

Implement a fake adapter transport.

Test:

```text
request 1
request 2

response 2
event
response 1
```

and verify request correlation works regardless of order.

## Engine tests

Feed synthetic events:

```text
Stopped
Continued
Terminated
```

and assert state transitions.

## Test-provider tests

Use fixture repositories for:

```text
Rust
.NET
```

Test:

- discovery;
- generated launch target;
- ambiguous selectors;
- missing toolchains.

## Integration tests

Run real adapters only in a smaller integration-test suite.

---

# 30. v0.1 milestone

The first useful release is complete when this works reliably:

```console
$ ddbg

Detected Rust workspace: my-app

ddbg> tests parser
1 parser::tests::invalid_input

ddbg> b src/parser.rs:52
Breakpoint 1 set

ddbg> td 1

Building tests...
Starting parser::tests::invalid_input...

Breakpoint 1
src/parser.rs:52

52 > let token = parse_token(input)?;

ddbg> p input
"foobar"

ddbg> n

53   validate(token)?;

ddbg> bt
#0 parse_token
#1 Parser::parse
#2 parser::tests::invalid_input

ddbg> c

test parser::tests::invalid_input ... ok

Process exited normally.
```

And the equivalent operation works for an MTP-based .NET test project.

---

# 31. Suggested implementation order

### Milestone 1 — DAP transport

Implement:

```text
codec
adapter process
request/response correlation
events
initialize
disconnect
```

Test against `lldb-dap`.

Success criterion:

```console
ddbg adapter-test lldb-dap
```

can initialize and print adapter capabilities.

### Milestone 2 — basic executable debugging

Implement:

```text
launch
break
continue
next
step
threads
stackTrace
scopes
variables
evaluate
```

Success criterion:

debug a simple Rust executable.

### Milestone 3 — REPL

Add:

```text
reedline
command parser
aliases
history
async event printing
```

At this point the tool should already feel like a small debugger.

### Milestone 4 — Cargo test provider

Implement:

```text
cargo metadata
cargo test --no-run
artifact extraction
test discovery
test-debug
```

This produces the first defining feature of `ddbg`.

### Milestone 5 — .NET/MTP

Implement:

```text
project detection
MTP discovery/execution
netcoredbg integration
test-debug
```

### Milestone 6 — polish

Add:

```text
configuration
completion
better diagnostics
adapter discovery
installation documentation
```

### Milestone 7 — TUI

Only after the command-based debugger is pleasant to use.

---

# 32. Key architectural rules

These should be treated as invariants for the project:

1. Frontends never communicate with DAP directly.

2. Test providers never communicate with DAP directly.

3. A test provider converts a test into a `DebugTarget`.

4. Debug adapters convert generic `DebugTarget`s into adapter-specific DAP launch/attach arguments.

5. The debug engine owns session state.

6. DAP `variablesReference` values never survive a resume.

7. Capabilities, not adapter names, determine whether generic DAP features are available.

8. CLI aliases exist only in the CLI.

9. Rust integration uses Cargo's public machine-readable interfaces, not filesystem assumptions. Cargo explicitly provides metadata and JSON compiler artifacts for external tools.

10. The TUI is a view over the existing debugger engine, never a second implementation.

---

# 33. Definition of the project

The shortest description of `ddbg` should be:

> **A terminal-first, language-agnostic debugger with first-class test debugging, built on DAP.**

The differentiating workflow is:

```console
ddbg> test-debug <test>
```

not simply:

```console
ddbg> launch <binary>
```

Everything in the architecture should make that workflow fast, predictable, and independent of the underlying language ecosystem.
