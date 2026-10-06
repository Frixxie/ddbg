//! Text → [`Command`] parsing. Aliases exist only here.

use ddbg_core::breakpoint::{BreakpointId, FunctionLocation, SourceLocation};
use ddbg_core::command::{Command, FrameSelector, Location, TestQuery, TestSelector};
use ddbg_core::thread::ThreadId;
use ddbg_core::{AttachTarget, LaunchTarget};

/// A parsed REPL line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    Empty,
    Help(Option<String>),
    Command(Command),
}

pub struct CommandSpec {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub usage: &'static str,
    pub help: &'static str,
}

pub const COMMANDS: &[CommandSpec] = &[
    spec(
        "run",
        &["r"],
        "run [program [args...]]",
        "Start the program (restarts if running)",
    ),
    spec(
        "attach",
        &[],
        "attach <pid>",
        "Attach to an existing local process using the session's adapter",
    ),
    spec(
        "detach",
        &[],
        "detach",
        "Disconnect and leave the process running",
    ),
    spec("continue", &["c"], "continue", "Resume execution"),
    spec(
        "pause",
        &[],
        "pause",
        "Interrupt the running program (also Ctrl-C)",
    ),
    spec("kill", &["k"], "kill", "Terminate the debugged program"),
    spec("next", &["n"], "next", "Step over"),
    spec("step", &["s"], "step", "Step into"),
    spec(
        "finish",
        &["fin"],
        "finish",
        "Step out of the current function",
    ),
    spec(
        "break",
        &["b"],
        "break <location> [if <expression>]",
        "Set a line (file:line) or function ([file:]function) breakpoint with an optional condition",
    ),
    spec(
        "condition",
        &[],
        "condition <breakpoint> [expression]",
        "Change a breakpoint condition; omit the expression to clear it",
    ),
    spec(
        "delete",
        &["d"],
        "delete <breakpoint>",
        "Delete a breakpoint",
    ),
    spec(
        "breakpoints",
        &["info-breakpoints"],
        "breakpoints",
        "List breakpoints",
    ),
    spec(
        "backtrace",
        &["bt", "where"],
        "backtrace",
        "Show the call stack",
    ),
    spec("threads", &[], "threads", "List threads"),
    spec("thread", &[], "thread <id>", "Select a thread"),
    spec("frame", &["f"], "frame [n]", "Select or show a stack frame"),
    spec("up", &[], "up", "Select the calling frame"),
    spec("down", &[], "down", "Select the called frame"),
    spec(
        "print",
        &["p"],
        "print <expression>",
        "Evaluate an expression",
    ),
    spec(
        "eval",
        &["e"],
        "eval <expression>",
        "Evaluate in the adapter's REPL context (may have side effects)",
    ),
    spec(
        "set",
        &[],
        "set [var] <lvalue> = <value>",
        "Assign a value to a variable or expression",
    ),
    spec("locals", &[], "locals", "Show local variables"),
    spec(
        "watch",
        &[],
        "watch <expression>",
        "Keep an expression and refresh it at every stop",
    ),
    spec(
        "watches",
        &[],
        "watches",
        "List watch expressions and their current values",
    ),
    spec("unwatch", &[], "unwatch <id>", "Remove a watch expression"),
    spec("tests", &[], "tests [filter]", "Discover and number tests"),
    spec(
        "test-run",
        &["tr"],
        "test-run <test>",
        "Run a test by number or name",
    ),
    spec(
        "test-debug",
        &["td"],
        "test-debug [-b|--break] <test>",
        "Debug a test by number or name; -b breaks at the start of the test",
    ),
    spec("help", &["h"], "help [command]", "Show help"),
    spec("quit", &["q", "exit"], "quit", "Exit ddbg"),
];

const fn spec(
    name: &'static str,
    aliases: &'static [&'static str],
    usage: &'static str,
    help: &'static str,
) -> CommandSpec {
    CommandSpec {
        name,
        aliases,
        usage,
        help,
    }
}

/// Resolve a command word or alias to its canonical spec.
pub fn lookup(word: &str) -> Option<&'static CommandSpec> {
    COMMANDS
        .iter()
        .find(|c| c.name == word || c.aliases.contains(&word))
}

pub fn parse(line: &str) -> Result<Input, String> {
    let line = line.trim();
    if line.is_empty() {
        return Ok(Input::Empty);
    }
    let (word, rest) = match line.split_once(char::is_whitespace) {
        Some((w, r)) => (w, r.trim()),
        None => (line, ""),
    };
    let spec = lookup(word).ok_or_else(|| format!("unknown command `{word}`; try `help`"))?;
    let usage = || format!("usage: {}", spec.usage);
    let no_args = |cmd: Command| {
        if rest.is_empty() {
            Ok(Input::Command(cmd))
        } else {
            Err(usage())
        }
    };

    match spec.name {
        "run" => {
            let words = split_words(rest)?;
            let target = words
                .split_first()
                .map(|(program, args)| LaunchTarget::new(program, args.to_vec()));
            Ok(Input::Command(Command::Run(target)))
        }
        "attach" => {
            let pid = rest.parse::<u32>().map_err(|_| usage())?;
            if pid == 0 {
                return Err(usage());
            }
            Ok(Input::Command(Command::Attach(AttachTarget { pid })))
        }
        "detach" => no_args(Command::Detach),
        "continue" => no_args(Command::Continue),
        "pause" => no_args(Command::Pause),
        "kill" => no_args(Command::Kill),
        "next" => no_args(Command::Next),
        "step" => no_args(Command::Step),
        "finish" => no_args(Command::Finish),
        "break" => {
            let (location, condition) = split_break_condition(rest);
            let location = parse_location(location).ok_or_else(usage)?;
            Ok(Input::Command(match condition {
                Some(condition) if !condition.is_empty() => Command::ConditionalBreak {
                    location,
                    condition: condition.to_owned(),
                },
                Some(_) => return Err(usage()),
                None => Command::Break(location),
            }))
        }
        "condition" => {
            let (id, condition) = rest
                .split_once(char::is_whitespace)
                .map_or((rest, ""), |(id, expr)| (id, expr.trim()));
            let id = id.parse().map_err(|_| usage())?;
            Ok(Input::Command(Command::Condition {
                id: BreakpointId(id),
                condition: (!condition.is_empty()).then(|| condition.to_owned()),
            }))
        }
        "delete" => {
            let id = rest.parse().map_err(|_| usage())?;
            Ok(Input::Command(Command::DeleteBreakpoint(BreakpointId(id))))
        }
        "breakpoints" => no_args(Command::Breakpoints),
        "backtrace" => no_args(Command::Backtrace),
        "threads" => no_args(Command::Threads),
        "thread" => {
            let id = rest.parse().map_err(|_| usage())?;
            Ok(Input::Command(Command::Thread(ThreadId(id))))
        }
        "frame" => {
            let sel = if rest.is_empty() {
                FrameSelector::Current
            } else {
                FrameSelector::Index(rest.parse().map_err(|_| usage())?)
            };
            Ok(Input::Command(Command::Frame(sel)))
        }
        "up" => no_args(Command::Frame(FrameSelector::Up)),
        "down" => no_args(Command::Frame(FrameSelector::Down)),
        "print" => {
            if rest.is_empty() {
                return Err(usage());
            }
            Ok(Input::Command(Command::Print(rest.to_owned())))
        }
        "eval" => {
            if rest.is_empty() {
                return Err(usage());
            }
            Ok(Input::Command(Command::Eval(rest.to_owned())))
        }
        "set" => {
            let rest = set_body(rest);
            let (target, value) = split_assignment(rest).ok_or_else(usage)?;
            Ok(Input::Command(Command::Set {
                target: target.to_owned(),
                value: value.to_owned(),
            }))
        }
        "locals" => no_args(Command::Locals),
        "watch" => {
            if rest.is_empty() {
                return Err(usage());
            }
            Ok(Input::Command(Command::Watch(rest.to_owned())))
        }
        "watches" => no_args(Command::Watches),
        "unwatch" => {
            let id = rest.parse().map_err(|_| usage())?;
            Ok(Input::Command(Command::Unwatch(ddbg_core::watch::WatchId(
                id,
            ))))
        }
        "tests" => Ok(Input::Command(Command::Tests(TestQuery {
            filter: (!rest.is_empty()).then(|| rest.to_owned()),
        }))),
        "test-run" => Ok(Input::Command(Command::TestRun(
            parse_test_selector(rest).ok_or_else(usage)?,
        ))),
        "test-debug" => {
            let mut break_at_start = false;
            let mut test = Vec::new();
            for w in rest.split_whitespace() {
                match w {
                    "-b" | "--break" => break_at_start = true,
                    _ => test.push(w),
                }
            }
            Ok(Input::Command(Command::TestDebug {
                test: parse_test_selector(&test.join(" ")).ok_or_else(usage)?,
                break_at_start,
            }))
        }
        "help" => Ok(Input::Help((!rest.is_empty()).then(|| rest.to_owned()))),
        "quit" => no_args(Command::Quit),
        other => unreachable!("unhandled command {other}"),
    }
}

/// Split off the first standalone `if` after a breakpoint location. The
/// expression is kept verbatim, including quotes and language-specific syntax.
pub fn split_break_condition(s: &str) -> (&str, Option<&str>) {
    for (i, c) in s.char_indices() {
        if c.is_whitespace() {
            let rest = s[i..].trim_start();
            if let Some(expr) = rest.strip_prefix("if")
                && (expr.is_empty() || expr.starts_with(char::is_whitespace))
            {
                return (s[..i].trim_end(), Some(expr.trim_start()));
            }
        }
    }
    (s, None)
}

/// `file:line`, `file:function` or `function`.
///
/// Splits at the last single colon, so Windows drive letters and `::` in
/// qualified names (`file.rs:mod::func`, `ns::func`) work.
fn parse_location(s: &str) -> Option<Location> {
    let s = s.trim();
    let (file, rest) = match split_location(s) {
        Some((file, rest)) => (Some(file.trim()), rest.trim()),
        None => (None, s),
    };
    if file.is_some_and(str::is_empty) || rest.is_empty() {
        return None;
    }
    if rest.bytes().all(|b| b.is_ascii_digit()) {
        let line: u32 = rest.parse().ok().filter(|l| *l > 0)?;
        return Some(Location::Source(SourceLocation::new(file?, line)));
    }
    if rest.contains(['/', '\\']) || rest.contains(char::is_whitespace) {
        return None;
    }
    Some(Location::Function(FunctionLocation::new(
        rest,
        file.map(Into::into),
    )))
}

/// Split at the last `:` that is not part of `::`.
fn split_location(s: &str) -> Option<(&str, &str)> {
    let b = s.as_bytes();
    (0..b.len())
        .rev()
        .find(|&i| b[i] == b':' && b.get(i + 1) != Some(&b':') && (i == 0 || b[i - 1] != b':'))
        .map(|i| (&s[..i], &s[i + 1..]))
}

/// Strip gdb's optional `var`/`variable` keyword from a `set` command.
pub fn set_body(rest: &str) -> &str {
    for kw in ["variable", "var"] {
        if let Some(r) = rest.strip_prefix(kw)
            && r.starts_with(char::is_whitespace)
        {
            return r.trim_start();
        }
    }
    rest
}

/// Split `lhs = rhs` at the first `=` that is not part of a comparison
/// operator or inside quotes. Both sides must be non-empty.
pub fn split_assignment(s: &str) -> Option<(&str, &str)> {
    let b = s.as_bytes();
    let mut quote = None;
    for i in 0..b.len() {
        match (quote, b[i]) {
            (Some(q), c) if c == q && b.get(i.wrapping_sub(1)) != Some(&b'\\') => quote = None,
            (Some(_), _) => {}
            (None, b'"' | b'\'') => quote = Some(b[i]),
            (None, b'=') => {
                let prev = i.checked_sub(1).map(|j| b[j]);
                if b.get(i + 1) == Some(&b'=') || matches!(prev, Some(b'=' | b'!' | b'<' | b'>')) {
                    continue;
                }
                let (lhs, rhs) = (s[..i].trim(), s[i + 1..].trim());
                return (!lhs.is_empty() && !rhs.is_empty()).then_some((lhs, rhs));
            }
            _ => {}
        }
    }
    None
}

fn parse_test_selector(s: &str) -> Option<TestSelector> {
    if s.is_empty() {
        return None;
    }
    Some(match s.parse::<usize>() {
        Ok(n) => TestSelector::Index(n),
        Err(_) => TestSelector::Name(s.to_owned()),
    })
}

/// Split shell-style words, honouring single/double quotes and backslashes.
pub fn split_words(s: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '\\') | (None, '\\') => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
                in_word = true;
            }
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                in_word = true;
            }
            (None, c) if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            (None, c) => {
                cur.push(c);
                in_word = true;
            }
        }
    }
    if quote.is_some() {
        return Err("unterminated quote".into());
    }
    if in_word {
        words.push(cur);
    }
    Ok(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_attach_and_detach() {
        assert_eq!(cmd("attach 42"), Command::Attach(AttachTarget { pid: 42 }));
        assert_eq!(cmd("detach"), Command::Detach);
        for line in [
            "attach",
            "attach 0",
            "attach -1",
            "attach 4294967296",
            "attach 42 extra",
            "detach extra",
        ] {
            assert!(parse(line).is_err(), "{line}");
        }
    }
    use quickcheck_macros::quickcheck;

    #[quickcheck]
    fn quoted_words_roundtrip(words: Vec<String>) -> bool {
        // Double quotes preserve empty arguments and whitespace; escape both
        // characters that have special meaning inside double quotes.
        let text = words
            .iter()
            .map(|word| format!("\"{}\"", word.replace('\\', "\\\\").replace('"', "\\\"")))
            .collect::<Vec<_>>()
            .join(" ");
        split_words(&text).unwrap() == words
    }

    #[quickcheck]
    fn aliases_parse_like_canonical_commands(rest: String) -> bool {
        COMMANDS.iter().all(|spec| {
            let expected = parse(&format!("{} {rest}", spec.name));
            spec.aliases
                .iter()
                .all(|alias| parse(&format!("{alias} {rest}")) == expected)
        })
    }

    #[quickcheck]
    fn outer_whitespace_does_not_change_parsing(text: String) -> bool {
        parse(&format!(" \t\u{2003}{text}\r\n")) == parse(&text)
    }

    #[quickcheck]
    fn source_locations_roundtrip(parts: Vec<u16>, line: u32, windows: bool) -> bool {
        let parts = parts.iter().map(|n| format!("dir{n}")).collect::<Vec<_>>();
        let path = if windows {
            format!(
                "C:\\{}file.rs",
                parts.iter().map(|p| format!("{p}\\")).collect::<String>()
            )
        } else {
            format!(
                "/{}file.rs",
                parts.iter().map(|p| format!("{p}/")).collect::<String>()
            )
        };
        let location = SourceLocation::new(path, line.max(1));
        parse_location(&location.to_string()) == Some(Location::Source(location))
    }

    #[quickcheck]
    fn qualified_function_locations_roundtrip(parts: Vec<u16>, scoped: bool) -> bool {
        let name = parts
            .iter()
            .map(|n| format!("mod{n}::"))
            .collect::<String>()
            + "function";
        let location = FunctionLocation::new(name, scoped.then(|| "src/file.rs".into()));
        parse_location(&location.to_string()) == Some(Location::Function(location))
    }

    fn cmd(s: &str) -> Command {
        match parse(s).unwrap() {
            Input::Command(c) => c,
            other => panic!("expected command, got {other:?}"),
        }
    }

    #[test]
    fn aliases_map_to_canonical_commands() {
        assert_eq!(cmd("c"), Command::Continue);
        assert_eq!(cmd("n"), Command::Next);
        assert_eq!(cmd("s"), Command::Step);
        assert_eq!(cmd("fin"), Command::Finish);
        assert_eq!(cmd("bt"), Command::Backtrace);
        assert_eq!(cmd("q"), Command::Quit);
        assert_eq!(cmd("p x + 1"), Command::Print("x + 1".into()));
    }

    #[test]
    fn parses_breakpoints() {
        assert_eq!(
            cmd("b src/parser.rs:42"),
            Command::Break(Location::Source(SourceLocation::new("src/parser.rs", 42)))
        );
        assert_eq!(
            cmd("break C:\\src\\a.cs:7"),
            Command::Break(Location::Source(SourceLocation::new("C:\\src\\a.cs", 7)))
        );
        assert!(parse("b src/foo.rs").is_err());
        assert!(parse("b foo.rs:0").is_err());
        assert!(parse("b foo.rs:").is_err());
        assert!(parse("b :main").is_err());
        assert!(parse("b").is_err());
        assert_eq!(cmd("delete 3"), Command::DeleteBreakpoint(BreakpointId(3)));
    }

    fn func(name: &str, file: Option<&str>) -> Command {
        Command::Break(Location::Function(FunctionLocation::new(
            name,
            file.map(Into::into),
        )))
    }

    #[test]
    fn parses_function_breakpoints() {
        assert_eq!(cmd("b main"), func("main", None));
        assert_eq!(
            cmd("b app::parser::parse"),
            func("app::parser::parse", None)
        );
        assert_eq!(cmd("b src/main.rs:run"), func("run", Some("src/main.rs")));
        assert_eq!(
            cmd("break src/lib.rs:Parser::new"),
            func("Parser::new", Some("src/lib.rs"))
        );
        assert_eq!(
            cmd("b C:\\src\\a.cs:App.Program.Main"),
            func("App.Program.Main", Some("C:\\src\\a.cs"))
        );
        assert_eq!(cmd("b Foo.cs:Bar"), func("Bar", Some("Foo.cs")));
    }

    #[test]
    fn parses_conditional_breakpoints_and_condition_edits() {
        for (line, location, condition) in [
            (
                "b src/main.rs:42 if count > 10",
                Location::Source(SourceLocation::new("src/main.rs", 42)),
                "count > 10",
            ),
            (
                r#"break C:\src\a.cs:7 if name == "if : 7""#,
                Location::Source(SourceLocation::new(r"C:\src\a.cs", 7)),
                r#"name == "if : 7""#,
            ),
            (
                "b src/lib.rs:Parser::new\tif\tready && ns::ok()",
                Location::Function(FunctionLocation::new(
                    "Parser::new",
                    Some("src/lib.rs".into()),
                )),
                "ready && ns::ok()",
            ),
            (
                "b run if x if y else z",
                Location::Function(FunctionLocation::new("run", None)),
                "x if y else z",
            ),
        ] {
            assert_eq!(
                cmd(line),
                Command::ConditionalBreak {
                    location,
                    condition: condition.into()
                }
            );
        }
        assert_eq!(
            cmd("condition 2 x == 3"),
            Command::Condition {
                id: BreakpointId(2),
                condition: Some("x == 3".into())
            }
        );
        assert_eq!(
            cmd("condition 2"),
            Command::Condition {
                id: BreakpointId(2),
                condition: None
            }
        );
        assert_eq!(cmd("b if"), func("if", None));
        for line in [
            "b main if",
            "b main if  ",
            "b if x",
            "condition",
            "condition nope x",
        ] {
            assert!(parse(line).is_err(), "{line}");
        }
    }

    #[test]
    fn parses_run_with_args() {
        let Command::Run(Some(t)) = cmd(r#"run ./app --name "two words" a\ b"#) else {
            panic!()
        };
        assert_eq!(t.program, std::path::Path::new("./app"));
        assert_eq!(t.args, ["--name", "two words", "a b"]);
        assert_eq!(cmd("r"), Command::Run(None));
    }

    #[test]
    fn parses_frames_and_tests() {
        assert_eq!(cmd("frame"), Command::Frame(FrameSelector::Current));
        assert_eq!(cmd("f 2"), Command::Frame(FrameSelector::Index(2)));
        assert_eq!(cmd("up"), Command::Frame(FrameSelector::Up));
        assert_eq!(
            cmd("td 2"),
            Command::TestDebug {
                test: TestSelector::Index(2),
                break_at_start: false
            }
        );
        let brk = Command::TestDebug {
            test: TestSelector::Name("a::x".into()),
            break_at_start: true,
        };
        assert_eq!(cmd("td -b a::x"), brk);
        assert_eq!(cmd("test-debug a::x --break"), brk);
        assert!(parse("td -b").is_err());
        assert_eq!(
            cmd("tr parser::tests::x"),
            Command::TestRun(TestSelector::Name("parser::tests::x".into()))
        );
    }

    #[test]
    fn parses_set_and_eval() {
        let set = |t: &str, v: &str| Command::Set {
            target: t.into(),
            value: v.into(),
        };
        assert_eq!(cmd("set x = 5"), set("x", "5"));
        assert_eq!(cmd("set var p.x=a == b"), set("p.x", "a == b"));
        assert_eq!(cmd("set variable a[i] = \"=\""), set("a[i]", "\"=\""));
        assert_eq!(cmd("set vary = 1"), set("vary", "1"));
        assert!(parse("set x").is_err());
        assert!(parse("set x == 1").is_err());
        assert!(parse("set = 1").is_err());
        assert_eq!(cmd("e x += 1"), Command::Eval("x += 1".into()));
        assert!(parse("eval").is_err());
    }

    #[test]
    fn parses_watch_commands_and_preserves_expression_syntax() {
        use ddbg_core::watch::WatchId;
        assert_eq!(
            cmd(r#"watch name == "hello world" && x > 2"#),
            Command::Watch(r#"name == "hello world" && x > 2"#.into())
        );
        assert_eq!(cmd("watches"), Command::Watches);
        assert_eq!(cmd("unwatch 3"), Command::Unwatch(WatchId(3)));
        for line in [
            "watch",
            "unwatch",
            "unwatch nope",
            "watches x",
            "unwatch 1 extra",
        ] {
            assert!(parse(line).is_err(), "{line}");
        }
    }

    #[test]
    fn rejects_unknown_and_extra_args() {
        assert!(parse("frobnicate").is_err());
        assert!(parse("continue now").is_err());
        assert_eq!(parse("  ").unwrap(), Input::Empty);
        assert_eq!(parse("help b").unwrap(), Input::Help(Some("b".into())));
    }

    #[test]
    fn command_table_has_no_duplicate_names() {
        let mut all: Vec<&str> = COMMANDS
            .iter()
            .flat_map(|c| std::iter::once(c.name).chain(c.aliases.iter().copied()))
            .collect();
        let n = all.len();
        all.sort();
        all.dedup();
        assert_eq!(all.len(), n);
    }
}
