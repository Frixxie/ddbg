# ddbg Evaluation in RetinaIntegration-Service

## Latest Re-evaluation — Updated Build

**Re-run date:** 2026-10-06. The earlier evaluation is preserved below as historical evidence; this section supersedes its verdict and bug statuses.

**Verdict:** the updated build improves lifecycle control and adds useful process attachment. The previous kill/wait and startup-pause bugs no longer reproduce. Core source debugging still works. Selected string-valued theory rows remain unusable through `debug_test`: instead of running all rows, they now run **zero tests**. This was isolated to netcoredbg launch-argument handling with an exact display-name filter. The adapter's incorrect non-zero exit reporting also remains.

Use ddbg for focused diagnosis, including attachment to a locally started test process. Continue to verify execution counts and results with runner output and ordinary `dotnet test`, not debugger exit codes alone.

### Build Identification and Scope

- The refreshed MCP catalog exposes **31 tools**, up from 29; the additions are `attach` and `detach`.
- `ddbg --version` still prints `ddbg 0.1.0`. The version string alone does not distinguish the two builds; no previous binary fingerprint was recorded.
- Local CLI binary: `/Users/fredrik/.cargo/bin/ddbg`.
- Local CLI SHA-256: `075a4ea8f9c1b51e464db76131a7e56f0c63f0a7d9e790e4ff74285bc3a0b54d`. This identifies the installed CLI file, not independently the running MCP server image.
- netcoredbg remains `3.2.0-1` (`9744e1f`, Release), with .NET `10.0.12` and the same repository baseline.
- **All 31 exposed tools were called**, including both new operations with real disposable .NET test processes.
- The same build and baseline test commands succeeded again: zero build errors, four existing package vulnerability warnings, **39 DateTimeConverter tests passed**, and **10 ReconciliationProgram tests passed**.
- The same safe boundaries were retained: no source/configuration/package changes, no Oracle tests, no live CDR or service startup, and no real reconciliation jobs. The in-memory theory argument was changed and restored using a manual test-DLL launch because `debug_test` did not reach that theory.
- Only this root report was edited during the re-evaluation. Developer documentation was not changed or given a report reference.

### Comparison with Previous Findings

| Finding | Previous build | Updated build |
| --- | --- | --- |
| F1: selected theory-row debugging | Ran all four rows instead of one. | **Changed, not resolved:** tested string-valued rows now run zero tests. Raw netcoredbg reproduces the exact-filter failure; see details below. |
| F2: `wait` after `kill` | Claimed `running` after termination. | **Fixed in tested scenarios:** returned `terminated` on two kill/restart cycles. Inspection/resume correctly rejected a dead debuggee. |
| F3: pause before first stop | Failed with `no thread selected`. | **Fixed in tested scenarios:** paused successfully in two fresh zero-wait launches and resumed to one passing test. |
| F4: non-zero exit reported as zero | Real exit 2 appeared as 0; raw adapter reproduced it. | **Still reproducible:** direct exit 2, ddbg exit 0, and raw netcoredbg `exited.exitCode: 0`. |
| Process attachment | Not exposed in MCP. | **Working:** attached to a waiting test runner, hit source breakpoint, inspected locals, detached, and observed normal completion. Ending another attached session also preserved test execution. |

### Capability Re-run Results

- **Discovery and execution:** discovered all 39 date-converter cases with source locations. `run_test` passed one selected theory row, the timezone Fact, and the mocked async reconciliation Fact. Invalid test names still produced a useful error.
- **Breakpoints:** source, function, conditional, pending/verified status, deletion, and condition editing worked. The date condition stopped only on `2023-09-25`; changing it to `false` allowed completion. The theory method was launched explicitly through `run` for this check.
- **Stepping and inspection:** `next`, `step`, `finish`, locals, object expansion, REPL-context evaluation, and source-only stacks worked again. Invalid expressions/frame indices did not corrupt the session. Main/worker thread switching and caller/callee navigation worked.
- **Watches and mutation:** the timezone watch changed from `0` to `330` after assignment. An invalid watch preserved its evaluation error. Changing `year` from `2023` to `2024` immediately updated its watch; restoring `2023` allowed all four explicitly selected method rows to pass.
- **Async:** stepped into `ReconciliationProgram.ExecuteJob`, inspected the Moq proxy, job enum, and cancellation token, finished back to the await, and stepped to the assertion. As before, this does not establish behavior across genuinely suspended I/O or a thread hop.
- **Output:** stdout/stderr separation and incremental reads worked. `debug_test` still returned ANSI-colored xUnit text despite the launched command including `--no-ansi`; the resulting formatting remains a usability issue rather than a functional failure.
- **Launch and wait:** ambiguous binary selection still errors clearly, bare reconciliation binary names resolve, and restart works. Zero-wait resume followed by `wait` works. `wait` after normal exit now retains the exit state/code rather than changing to a generic terminal state.
- **Entry-stop ordering:** `start_session(stop_on_entry: true)` returned metadata before the entry stop was ready. Immediate frame/resume calls encountered `the program is running; use pause first`. Calling `wait` then observed the entry stop at `Program.cs:12`, after which inspection/resume worked. Treat startup as asynchronous; this is an ordering caveat, not an established failed entry-stop feature.
- **Pause:** successful startup pauses took approximately 650 ms and had no managed source frame yet (`frame: null`, empty stack). Continuing then completed the selected Fact successfully. A startup pause can therefore interrupt without yielding a useful managed stack at that instant.
- **REPL:** `help` now lists `attach` and `detach`; other command coverage remained unchanged.

Test breakpoint launches were approximately 1.4–1.8 seconds; simple steps were approximately 70–150 ms. These are observations, not benchmarks or evidence of a performance improvement over the previous build.

### F1 Re-test — Exact Theory Filter Runs Zero Tests

**Severity:** High for the affected debugging workflow, especially when combined with F4. **Attribution:** exact-display-name launch failure reproduced directly in netcoredbg; ddbg's updated theory selection exposes it. No implementation-level root cause was audited.

Reproduction in a fresh ddbg session:

1. `start_session({cwd: "<repository>"})`.
2. `list_tests({filter: "DateTimeConverterTests.ShouldParseValidDateStrings"})`.
3. `run_test({test: "1"})` reports one passing row.
4. `debug_test({test: "1", break_at_start: true, timeout_ms: 5000})`.
5. `get_output()`.

**Expected:** stop at the selected row's entry breakpoint and eventually report one executed test.

**Actual:** no breakpoint hit; ddbg returns `{"state":"exited","exit_code":0}`. The runner says:

```text
Test run summary: Zero tests ran
  total: 0
  failed: 0
  succeeded: 0
  skipped: 0
```

Reproduced for row 1 in multiple sessions and row 3 in a fresh session. Facts debug normally. This result is established for the tested string-valued date theories, not every possible theory shape.

**Isolation evidence:** launching the same test DLL directly with the full row name passed to `--filter-display-name` via Python's argument list ran one passing test. Passing the identical argument list to netcoredbg's raw DAP `launch` ran zero tests. Process inspection during a paused ddbg launch showed the display-name argument with the input's literal quotation marks absent:

```text
--filter-display-name DIPS.RetinaIntegration.Tests.Unit.DateTimeConverterTests.ShouldParseValidDateStrings(input: 2023-09-25T14:30:00+02:00, year: 2023, month: 9, day: 25, hour: 14, minute: 30, second: 0, offsetHours: 2)
```

The required exact display name contains `input: "2023-09-25T14:30:00+02:00"`. This supports a launch-argument quoting problem in the adapter path rather than missing tests or broken PDBs. The separate zero exit-code problem masks the no-tests failure in the resume result.

**Verified workaround:** set a source breakpoint at `DateTimeConverterTests.cs:15`, then launch the test DLL with `run` and quote-free simple filters:

```json
{
  "program": "<repository>/src/RetinaIntegration.Tests/bin/Debug/net10.0/DIPS.RetinaIntegration.Tests.dll",
  "args": [
    "--filter-method",
    "DIPS.RetinaIntegration.Tests.Unit.DateTimeConverterTests.ShouldParseValidDateStrings",
    "--filter-display-name",
    "*offsetHours: 2)",
    "--no-ansi",
    "--progress",
    "off"
  ],
  "timeout_ms": 5000
}
```

This hit the breakpoint with the expected input and completed with `total: 1`, `succeeded: 1`. Another verified fallback is `--filter-method` alone, which deliberately runs all four method rows and supports argument-conditional breakpoints. Filters must have wildcards only at their beginning/end; an exploratory filter with an internal wildcard was rejected by xUnit and is not a ddbg bug.

**Suggested fixes:** use discovered test UIDs for row isolation, or preserve literal quotes through the adapter-specific launch path. Add a string-argument theory regression test that checks both breakpoint entry and execution count. Surface zero-tests runner failures instead of allowing a zero adapter exit code to look successful.

### F2/F3 Re-test Evidence

- Two stopped-Fact `kill`/`wait` cycles returned `{"state":"terminated"}` immediately; restart then hit the Fact breakpoint again.
- Two fresh `debug_test({test: "1", timeout_ms: 0})` launches followed by `pause({timeout_ms: 5000})` returned `state: stopped`, `reason: pause`, and a thread ID, not `no thread selected`. Continuing each produced a one-test passing summary.
- Concurrency with a separate waiting resume call and long-running startup hangs remain untested. The fixes are confirmed for the original smoke reproductions, not every race condition.

### F4 Re-test Evidence

The no-argument reconciliation launch still exits with code **2** outside the debugger. Under ddbg it reports **0** both on a direct run and after stopping at the `return 2` source line. Stop-on-entry followed by `wait`, zero-wait `continue`, and another `wait` gives the same incorrect zero.

A new raw-DAP check independently reproduced netcoredbg's `{"body":{"exitCode":0},"event":"exited","type":"event"}`. The adapter version is unchanged. This remains an adapter/runtime issue rather than evidence that ddbg changes the emitted value.

### New Capability — Attach, Detach, and Attached-Session Shutdown

Two disposable processes were started locally using the existing test DLL, a filter selecting only `ShouldParseTimezoneWithMinuteOffset`, and the runner's `--debug` option. They printed their PID and waited for a debugger; no application fixture was introduced.

1. Start a session for the repository/test DLL and set the source breakpoint at line 107.
2. `attach({pid: <owned-test-process>, timeout_ms: 5000})` reached that breakpoint.
3. Frame and local inspection worked in the attached process.
4. `detach()` returned `Detached; the process is left running.` The external runner then completed with one passing test, and its PID no longer existed. ddbg `wait` returned `terminated`, describing the ended debugger relationship rather than the external process's liveness.
5. Repeat with a second process and call `end_session()` while attached and stopped. Its external log also showed one passing test afterward, confirming that session shutdown did not kill it.

This is valuable for debugging a test/application started with its own environment and arguments. Attachment to arbitrary already-running service hosts and detach while a separate tool call is waiting were not tested.

**Cleanup:** all re-evaluation debug sessions were ended. Both disposable attached processes completed normally; no evaluation-owned debuggee was left running. The original coverage gaps below otherwise remain applicable.

---

## Initial Evaluation — Historical Results

The following describes the previous build only. For current status, use the latest re-evaluation above.

## Verdict

ddbg is useful for agent-driven, source-level debugging of this project's .NET code. It successfully discovered xUnit v3 tests, debugged synchronous and async tests, stepped into production code, inspected arguments and objects, refreshed watches, and changed runtime values without editing source.

It is not yet reliable as an unattended execution controller or pass/fail authority in the tested configuration. Three reproducible ddbg-facing problems concern theory-row selection, state after termination, and pausing before the first stop. A separate netcoredbg/runtime problem makes a non-zero process exit appear successful.

**Recommendation:** use ddbg for focused diagnosis with explicit breakpoints; use the ordinary test runner to verify the result. Do not trust debugger exit codes alone.

## Environment and Scope

- Evaluation date: 2026-10-06.
- Platform: macOS 26.6, Apple Silicon (`osx-arm64`).
- ddbg: `0.1.0`, accessed through its MCP tools.
- Adapter: netcoredbg `3.2.0-1` (`9744e1f`, Release).
- SDK selected by the repository: .NET `10.0.401`.
- Target/runtime: `net10.0`, .NET `10.0.12`.
- Tests: xUnit v3 `4.0.1`, Microsoft.Testing.Platform runner.
- Repository baseline: parent commit `0c35e8b4`; existing unrelated SQL/documentation changes were preserved.
- No production code, test source, package versions, or runtime configuration was changed. A theory argument was temporarily changed in debugger memory and restored before continuing.
- No Oracle-backed tests, live CDR requests, web service startup, or actual reconciliation jobs were run. Reconciliation was launched only with no arguments, which exits before host creation.

All 29 exposed ddbg tools were called. This is a functional smoke evaluation, not exhaustive coverage of every option, language, platform, or concurrency scenario.

## Baseline Verification

```bash
dotnet build --no-restore
dotnet test --project src/RetinaIntegration.Tests/DIPS.RetinaIntegration.Tests.csproj --no-build -- --filter-class '*DateTimeConverterTests'
dotnet test --project src/RetinaIntegration.Tests/DIPS.RetinaIntegration.Tests.csproj --no-build -- --filter-class '*ReconciliationProgramTests'
```

- Build succeeded with zero errors. Four existing NuGet warnings concerned AutoMapper and KubernetesClient vulnerabilities; these are not debugger findings.
- DateTimeConverterTests: **39 passed**, zero failed/skipped.
- ReconciliationProgramTests: **10 passed**, zero failed/skipped.
- ddbg `run_test` separately passed the selected timezone fact, one date-parsing theory row, and the mocked reconciliation command fact.

## Capability Results

| Capability / tools | Observed result |
| --- | --- |
| Project detection: `start_session` | Correctly detected .NET and offered the service and reconciliation binaries. Multiple binaries require explicit selection; an ambiguous `run` returned a clear error. |
| Launch/restart: `run`, `start_session(stop_on_entry: true)` | Bare reconciliation binary name resolved correctly; restart worked. Stop-on-entry stopped in `Program.cs` at line 12. The start response itself contained session metadata, not the stopped frame; `select_frame` retrieved it. |
| Discovery: `list_tests` | Found all 39 DateTimeConverter cases with theory values, source paths, line numbers, and indices. Filtering worked. Indices refer to the most recent discovery list. |
| Test execution: `run_test` | Ran exactly one selected theory row and reported `1 passed, 0 failed, 0 ignored`. Unknown test names produced a useful error. |
| Test debugging: `debug_test` | Fact and async entry breakpoints worked. Selecting a theory row unexpectedly debugged the entire method; see F1. |
| Source/function breakpoints: `set_breakpoint`, `list_breakpoints`, `delete_breakpoint` | Relative source paths resolved correctly. Pending breakpoints became verified after code loaded. A fully qualified converter function breakpoint hit successfully. |
| Conditions: `set_breakpoint_condition` | A source condition on `dateString == "2023-09-25"` stopped only on that input. Changing the condition to `false` allowed completion without further hits. |
| Execution: `next`, `step`, `finish`, `continue` | Stepped from a test into DateTimeConverter and returned to the caller. Also stepped into async `ReconciliationProgram.ExecuteJob` and back across the await. |
| Inspection: `locals`, `evaluate` | Correct string/integer arguments, DateTimeOffset and TimeSpan members, Moq proxies, enum values, and CancellationToken data. Evaluation returned one level of children. Invalid expressions failed clearly without breaking the session. REPL-context evaluation also worked. |
| Stack/frame/thread: `backtrace`, `select_frame`, `threads`, `select_thread` | Useful source-only stack with hidden-framework-frame counts. Caller/callee navigation and switching from worker to main thread and back worked. An invalid frame index preserved the current frame. |
| Watches: `add_watch`, `list_watches`, `remove_watch` | `result.Offset.TotalMinutes` changed from `0` to `330` after the assignment. Invalid watches retained their evaluation error without preventing execution. |
| Mutation: `set_value` | Changed theory parameter `year` from `2023` to `2024`; `evaluate` confirmed the change. Restored it to `2023` before assertions. |
| Output: `get_output` | Correct stdout/stderr separation and xUnit summary. A second immediate read returned empty output, confirming incremental consumption. ANSI escape sequences remain in returned text. |
| Asynchronous waiting: `wait` | A zero-wait `continue` returned `running`; `wait` subsequently returned `exited`. State after explicit kill was incorrect; see F2. |
| Interruption: `pause` | Failed before the first stop with `no thread selected`; reproduced in two fresh sessions. See F3. Successful pause after a prior stop was not established. |
| Lifecycle: `kill`, `end_session` | Kill stopped the debuggee and the session could launch another test. Session shutdown/recreation worked. `wait` after kill incorrectly claimed the program was running. |
| Escape hatch: `repl` | `help` returned the command list. Arbitrary native debugger commands were not evaluated. |

Observed launches to test breakpoints were approximately 1.4–1.9 seconds, with many simple steps approximately 70–160 milliseconds. These are individual tool observations, not a controlled benchmark. Evaluation and async steps can take longer.

### Source-Stepping Details

`finish` returned to the caller's call-site line before the destination local had been assigned. One additional `next` reached the assertion and refreshed the offset watch to `330`. This is normal sequence-point behavior, not evidence of a stale-watch bug.

Async source locations and locals were usable, but stack names exposed compiler-generated `MoveNext` methods. The mocked command completed without establishing a real suspended-I/O continuation or thread hop; those cases remain unverified.

## Reproducible Findings

The examples below use ddbg MCP tool arguments. Start sessions with the absolute repository directory as `cwd`. Breakpoint IDs are assigned dynamically: use the returned ID rather than assuming the IDs from this evaluation.

### F1 — Debugging a Selected Theory Row Runs Every Row

**Severity:** Medium. **Attribution:** ddbg test-selection behavior; implementation cause not audited.

Reproduction:

1. `start_session({cwd: "<repository>"})`.
2. `list_tests({filter: "DateTimeConverterTests.ShouldParseValidDateStrings"})`.
3. `run_test({test: "1"})` reports one passing row.
4. `debug_test({test: "1", break_at_start: true, timeout_ms: 5000})`.
5. Inspect `input`, then repeatedly `continue` and inspect it at each entry stop.
6. Read the final test summary with `get_output`.

**Expected:** debug only the selected discovery result, consistent with `run_test`.

**Actual:** all four method rows execute. Entry stops show, in order:

```text
2023-09-25T14:30:00+02:00
2023-09-25T12:30:00Z
2023-09-25
2020-02-29T23:59:59+00:00
```

The debug summary says `total: 4`, `succeeded: 4`. A subsequent conditional-breakpoint run also executed all four rows.

**Impact:** unwanted cases can hit breakpoints, slow debugging, or perform additional setup/side effects. This matters particularly for database-backed theories.

**Workaround:** prefer a Fact for focused diagnosis, or use an argument-conditional breakpoint and assume the other theory rows still execute. A conditional breakpoint does not isolate test execution.

**Suggested fix:** retain the discovered row identifier when constructing the debug runner filter, or explicitly disclose method-wide selection if the runner cannot isolate it.

### F2 — `wait` Reports Running After `kill`

**Severity:** Medium. **Attribution:** ddbg lifecycle/state reporting.

Reproduction:

1. Discover and debug `DateTimeConverterTests.ShouldParseTimezoneWithMinuteOffset` with `break_at_start: true`.
2. While stopped, call `kill()`; it returns `Killed.`.
3. Call `wait({timeout_ms: 1000})`.
4. Call `threads()` or `continue({timeout_ms: 500})`.

**Expected:** a terminal state from `wait`, or a clear error that the program is not running.

**Actual:** `wait` returns `{"state":"running","timeout_ms":1000}`, while both inspection/resume commands say `the program is not being run`.

Reproduced again with the async reconciliation fact and `wait({timeout_ms: 500})`.

**Impact:** orchestration can keep waiting for a dead process or make incorrect recovery decisions.

**Workaround:** treat successful `kill` as terminal, then restart explicitly or end the session. Do not use post-kill `wait` to determine liveness.

**Suggested fix:** retain explicit termination state independently of adapter event timing and consult it before waiting.

### F3 — Cannot Pause a Fresh Launch Before the First Stop

**Severity:** Medium. **Attribution:** ddbg thread selection/interruption behavior.

Reproduction, confirmed in two fresh sessions:

1. `start_session({cwd: "<repository>"})`.
2. `list_tests({filter: "ShouldParseTimezoneWithMinuteOffset"})`.
3. `debug_test({test: "1", timeout_ms: 0})` returns `{"state":"running","timeout_ms":0}`.
4. Immediately call `pause({timeout_ms: 5000})`.

**Expected:** pause selects an available thread and interrupts, waits for a thread to become available, or reports that the process exited.

**Actual:** `Error: no thread selected`. `backtrace` and `continue` then report `the program is running; use pause first`. In the second reproduction, `wait` recovered and observed normal test completion.

**Impact:** the advertised pause operation cannot interrupt this startup state. It may obstruct diagnosis of hangs before an initial breakpoint; a long-running startup hang was not tested.

**Workaround:** launch with an entry breakpoint (`break_at_start` or `stop_on_entry`) when possible. On this error, use `wait` or terminate instead of retrying inspection commands.

**Suggested fix:** discover/select a live thread during pause rather than requiring a previously stopped thread, handling launch-time races explicitly.

### F4 — Non-Zero Process Exit Appears as Zero in netcoredbg

**Severity:** High for automated success/failure decisions. **Attribution:** reproduced directly in netcoredbg, independent of ddbg.

Reproduction:

```bash
dotnet src/RetinaIntegration.Reconciliation/bin/Debug/net10.0/DIPS.RetinaIntegration.Reconciliation.dll
```

With no arguments, the program prints its usage and exits with **code 2**, as specified by `src/RetinaIntegration.Reconciliation/Program.cs:16`.

In ddbg:

1. `start_session({cwd: "<repository>"})`.
2. `run({program: "DIPS.RetinaIntegration.Reconciliation", timeout_ms: 5000})`.
3. Observe `{"state":"exited","exit_code":0}` and the same usage text on stderr.

This occurred on repeated launches, after a breakpoint on the `return 2` line, and after stop-on-entry followed by `continue`/`wait`.

**Adapter isolation:** a separate Python subprocess spoke DAP directly to `netcoredbg --interpreter=vscode`, issuing `initialize`, `launch` (same DLL, empty args, internal console), and `configurationDone`. The raw adapter emitted:

```json
{"body":{"exitCode":0},"event":"exited","type":"event"}
```

Thus ddbg's reported zero is consistent with the adapter event. This does not establish the underlying netcoredbg/runtime cause, but it rules out attributing the observed value solely to ddbg's event conversion.

**Impact:** unsuccessful executions can look successful. The effect on arbitrary failing xUnit runs was not separately established.

**Workaround:** confirm process exit codes outside the debugger; verify tests with `dotnet test` and inspect runner summaries. Do not interpret debugger code zero as proof of success.

**Suggested follow-up:** reproduce against another netcoredbg build/runtime and report upstream with the raw DAP evidence. No upstream issue was filed during this evaluation.

## Practical Use in This Repository

1. Build Debug binaries before debugging to avoid stale binaries/PDBs.
2. Discover a narrowly filtered test and use its full name when later discovery calls might change the indices.
3. Prefer isolated unit tests over tests that initialize Oracle or live clients.
4. Use `break_at_start` to establish a stopped thread, then source or function breakpoints in the domain/handler under investigation.
5. Use conditional breakpoints for specific payloads; remember that theory-row debug selection currently runs the other rows too.
6. Inspect scalar properties first to keep output manageable. Full object evaluation can return many framework members.
7. Read stdout/stderr explicitly with `get_output`; resume results do not contain test summaries.
8. Finish by ending the session and rerunning the focused tests outside the debugger.

The runner explicitly warned that test timeouts and long-running test detection are disabled while a debugger is attached. Tool timeout values control how long a call waits; they do not constitute a test-execution deadline.

## Remaining Coverage Gaps

- Real async suspension, continuation on a different thread, concurrent breakpoints, and pause while another resume call is waiting.
- Exception break policies and unhandled-exception inspection. The exposed tools do not include a dedicated exception-breakpoint configuration operation; raw adapter configuration was not explored for this purpose.
- Service HTTP requests, LightInject-created handlers, Oracle access, and long-lived background messaging.
- Failure/skip handling in `run_test`, complex assignments, native REPL commands, and every breakpoint-condition expression variant.
- Explicit adapter overrides, `no_detect`, other languages/operating systems, and alternate netcoredbg/runtime versions.

These are untested areas, not confirmed failures. All evaluation debug sessions were ended.
