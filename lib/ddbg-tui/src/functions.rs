//! Static discovery of functions to break on.
//!
//! DAP has no request to enumerate functions, so sources under the working
//! directory are scanned with a line-based heuristic. It assumes formatted
//! code (rustfmt / dotnet format): declarations start their own line and
//! enclosing types are tracked by indentation. Misses are acceptable; the
//! result only feeds a picker.

use std::path::{Path, PathBuf};

use crate::picker::PickerItem;

/// Directories never worth scanning.
const SKIP_DIRS: &[&str] = &["target", "bin", "obj", "node_modules"];
/// Stop scanning huge trees.
const MAX_FILES: usize = 5000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Function {
    /// Bare name, used for the function breakpoint.
    pub name: String,
    /// Name qualified by enclosing types, for display.
    pub qualified: String,
    pub path: PathBuf,
    /// 1-based line of the declaration.
    pub line: u32,
    /// `qualified  relative/path`, what the filter matches.
    pub label: String,
}

impl PickerItem for Function {
    fn label(&self) -> &str {
        &self.label
    }
}

#[derive(Clone, Copy)]
enum Lang {
    Rust,
    CSharp,
}

/// Scan source files under `root`, sorted by path then line.
pub fn discover(root: &Path) -> Vec<Function> {
    let mut files = Vec::new();
    collect_files(root, &mut files);
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut out = Vec::new();
    for (path, lang) in files {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let rel = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
        for (name, qualified, line) in scan(&text, lang) {
            out.push(Function {
                label: format!("{qualified}  {}", rel.display()),
                name,
                qualified,
                path: path.clone(),
                line,
            });
        }
    }
    out
}

fn collect_files(dir: &Path, out: &mut Vec<(PathBuf, Lang)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if out.len() >= MAX_FILES {
            return;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') {
            continue;
        }
        let Ok(ty) = entry.file_type() else { continue };
        let path = entry.path();
        if ty.is_dir() {
            if !SKIP_DIRS.contains(&name.as_ref()) {
                collect_files(&path, out);
            }
        } else if ty.is_file() {
            let lang = match path.extension().and_then(|e| e.to_str()) {
                Some("rs") => Lang::Rust,
                Some("cs") => Lang::CSharp,
                _ => continue,
            };
            out.push((path, lang));
        }
    }
}

/// `(name, qualified, line)` for each function declared in `text`.
fn scan(text: &str, lang: Lang) -> Vec<(String, String, u32)> {
    // Enclosing types as (indent, name).
    let mut types: Vec<(usize, String)> = Vec::new();
    let mut out = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim_start();
        if line.is_empty()
            || line.starts_with("//")
            || line.starts_with('#')
            || line.starts_with('[')
            || line.starts_with('{')
            || line.starts_with("where")
        {
            continue;
        }
        let indent = raw.len() - line.len();
        while types.last().is_some_and(|(ind, _)| *ind >= indent) {
            types.pop();
        }
        if line.starts_with('}') {
            continue;
        }
        let (ty, func) = match lang {
            Lang::Rust => (rust_type(line), rust_fn(line)),
            Lang::CSharp => (cs_type(line), cs_method(line)),
        };
        if let Some(ty) = ty {
            types.push((indent, ty));
        } else if let Some(name) = func {
            let sep = match lang {
                Lang::Rust => "::",
                Lang::CSharp => ".",
            };
            let mut qualified: Vec<&str> = types.iter().map(|(_, t)| t.as_str()).collect();
            qualified.push(&name);
            out.push((name.clone(), qualified.join(sep), i as u32 + 1));
        }
    }
    out
}

fn ident(s: &str) -> Option<&str> {
    let end = s
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(s.len());
    let id = &s[..end];
    (!id.is_empty() && !id.starts_with(|c: char| c.is_ascii_digit())).then_some(id)
}

/// Strip leading words that may precede `fn`, `impl`, `mod` etc.
fn strip_rust_modifiers(mut s: &str) -> &str {
    loop {
        let before = s;
        if let Some(rest) = s.strip_prefix("pub") {
            s = rest.trim_start();
            if let Some(rest) = s.strip_prefix('(') {
                s = rest.split_once(')').map_or("", |(_, r)| r).trim_start();
            }
        }
        for kw in ["const ", "async ", "unsafe ", "default ", "extern "] {
            if let Some(rest) = s.strip_prefix(kw) {
                s = rest.trim_start();
            }
        }
        if s.starts_with('"') {
            // ABI string of `extern "C"`.
            s = s[1..].split_once('"').map_or("", |(_, r)| r).trim_start();
        }
        if s == before {
            return s;
        }
    }
}

fn rust_fn(line: &str) -> Option<String> {
    let rest = strip_rust_modifiers(line).strip_prefix("fn ")?;
    ident(rest.trim_start()).map(str::to_owned)
}

/// `impl ... [for] Type`, `mod name`, `trait Name` opening a block.
fn rust_type(line: &str) -> Option<String> {
    let s = strip_rust_modifiers(line);
    if let Some(rest) = s.strip_prefix("impl") {
        if !rest.starts_with([' ', '<']) {
            return None;
        }
        let rest = skip_generics(rest.trim_start()).trim_start();
        let rest = rest
            .split_once(" for ")
            .map_or(rest, |(_, ty)| ty)
            .trim_start();
        let rest = rest
            .trim_start_matches(['&', '*'])
            .trim_start_matches("dyn ");
        // Last path segment: `a::B<T>` -> `B`.
        let path_end = rest.find(['<', ' ', '{', '(']).unwrap_or(rest.len());
        let last = rest[..path_end].rsplit("::").next()?;
        return ident(last).map(str::to_owned);
    }
    for kw in ["mod ", "trait "] {
        if let Some(rest) = s.strip_prefix(kw)
            && !line.trim_end().ends_with(';')
        {
            return ident(rest.trim_start()).map(str::to_owned);
        }
    }
    None
}

fn skip_generics(s: &str) -> &str {
    if !s.starts_with('<') {
        return s;
    }
    let mut depth = 0;
    for (i, c) in s.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => {
                depth -= 1;
                if depth == 0 {
                    return &s[i + 1..];
                }
            }
            _ => {}
        }
    }
    ""
}

const CS_MODIFIERS: &[&str] = &[
    "public",
    "private",
    "protected",
    "internal",
    "static",
    "virtual",
    "override",
    "abstract",
    "sealed",
    "async",
    "extern",
    "unsafe",
    "partial",
    "new",
    "readonly",
    "file",
];

const CS_TYPE_KEYWORDS: &[&str] = &["class", "struct", "record", "interface"];

/// Keywords that look like `word (` but are not declarations.
const CS_STATEMENTS: &[&str] = &[
    "if", "for", "foreach", "while", "switch", "return", "using", "catch", "lock", "else", "throw",
    "await", "var", "new", "nameof", "typeof", "sizeof", "default", "when", "yield", "fixed",
    "base", "this", "case", "do", "try", "get", "set", "init", "delegate", "event",
];

fn cs_type(line: &str) -> Option<String> {
    let mut words = line.split_whitespace();
    for w in words.by_ref() {
        if CS_TYPE_KEYWORDS.contains(&w) {
            return words.next().and_then(ident).map(str::to_owned);
        }
        if !CS_MODIFIERS.contains(&w) {
            return None;
        }
    }
    None
}

fn cs_method(line: &str) -> Option<String> {
    let line = line.trim_end();
    if line.ends_with(';') && !line.contains("=>") {
        return None; // abstract/interface member or a call statement
    }
    let open = line.find('(')?;
    let head = &line[..open];
    if head.contains(['=', '.', '"', '?', ':']) && !head.contains("operator") {
        return None;
    }
    // Strip generic parameters on the name: `Foo<T>(`.
    let head = match head.trim_end().strip_suffix('>') {
        Some(h) => h.rsplit_once('<')?.0,
        None => head,
    };
    let mut words: Vec<&str> = head.split_whitespace().collect();
    let name = ident(words.pop()?)?;
    let first = *words.first()?;
    if CS_STATEMENTS.contains(&first) || CS_STATEMENTS.contains(&name) {
        return None;
    }
    // Needs a modifier or a return type before the name.
    let has_modifier = words.iter().any(|w| CS_MODIFIERS.contains(w));
    if !has_modifier && words.len() != 1 {
        return None;
    }
    Some(name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(text: &str, lang: Lang) -> Vec<(String, u32)> {
        scan(text, lang)
            .into_iter()
            .map(|(_, q, l)| (q, l))
            .collect()
    }

    #[test]
    fn rust_functions_and_impls() {
        let src = r#"
fn main() {
    let x = foo();
}

pub(crate) async fn fetch() {}

impl<T: Clone> fmt::Display for Wrapper<T> {
    fn fmt(&self) {}
}

impl Parser {
    pub const fn new() -> Self {
        Self
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn parses() {}
}

fn after() {}
"#;
        let got = names(src, Lang::Rust);
        assert_eq!(
            got,
            [
                ("main".into(), 2),
                ("fetch".into(), 6),
                ("Wrapper::fmt".into(), 9),
                ("Parser::new".into(), 13),
                ("tests::parses".into(), 21),
                ("after".into(), 24),
            ]
        );
    }

    #[test]
    fn csharp_methods() {
        let src = r#"
namespace HelloTests;

public static class Calculator
{
    public static int Add(int a, int b)
    {
        var sum = Other(a, b);
        if (sum > 0) { return sum; }
        return sum;
    }

    private Task<List<int>> Load<T>(T x) => Task.FromResult(new List<int>());

    public Calculator(int seed)
    {
    }
}

public class CalculatorTests
{
    [Fact]
    public void Adds()
    {
        Assert.Equal(5, Calculator.Add(2, 3));
    }
}
"#;
        let got = names(src, Lang::CSharp);
        assert_eq!(
            got,
            [
                ("Calculator.Add".into(), 6),
                ("Calculator.Load".into(), 13),
                ("Calculator.Calculator".into(), 15),
                ("CalculatorTests.Adds".into(), 23),
            ]
        );
    }

    #[test]
    fn discovers_fixture() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/hello-rust");
        let fns = discover(&root);
        assert!(fns.iter().any(|f| f.name == "main"), "{fns:?}");
    }
}
