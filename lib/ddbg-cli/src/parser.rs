//! Text → [`Command`] parsing. Aliases exist only here.

use ddbg_core::LaunchTarget;
use ddbg_core::breakpoint::{BreakpointId, FunctionLocation, SourceLocation};
use ddbg_core::command::{Command, FrameSelector, Location, TestQuery, TestSelector};
use ddbg_core::thread::ThreadId;

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
        "break <file>:<line> | [<file>:]<function>",
        "Set a breakpoint at a line or function",
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
    spec("locals", &[], "locals", "Show local variables"),
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
        "test-debug <test>",
        "Debug a test by number or name",
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
        "continue" => no_args(Command::Continue),
        "pause" => no_args(Command::Pause),
        "kill" => no_args(Command::Kill),
        "next" => no_args(Command::Next),
        "step" => no_args(Command::Step),
        "finish" => no_args(Command::Finish),
        "break" => {
            let loc = parse_location(rest).ok_or_else(usage)?;
            Ok(Input::Command(Command::Break(loc)))
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
        "locals" => no_args(Command::Locals),
        "tests" => Ok(Input::Command(Command::Tests(TestQuery {
            filter: (!rest.is_empty()).then(|| rest.to_owned()),
        }))),
        "test-run" => Ok(Input::Command(Command::TestRun(
            parse_test_selector(rest).ok_or_else(usage)?,
        ))),
        "test-debug" => Ok(Input::Command(Command::TestDebug(
            parse_test_selector(rest).ok_or_else(usage)?,
        ))),
        "help" => Ok(Input::Help((!rest.is_empty()).then(|| rest.to_owned()))),
        "quit" => no_args(Command::Quit),
        other => unreachable!("unhandled command {other}"),
    }
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
        assert_eq!(cmd("td 2"), Command::TestDebug(TestSelector::Index(2)));
        assert_eq!(
            cmd("tr parser::tests::x"),
            Command::TestRun(TestSelector::Name("parser::tests::x".into()))
        );
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
