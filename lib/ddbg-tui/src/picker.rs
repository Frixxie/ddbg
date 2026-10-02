//! Pickers: filterable lists of discovered tests or programs.

use std::path::{Path, PathBuf};

use ddbg_cli::TestCase;

/// Something a [`Picker`] can list and filter.
pub trait PickerItem {
    /// Text the filter matches against.
    fn label(&self) -> &str;
}

impl PickerItem for TestCase {
    fn label(&self) -> &str {
        &self.name
    }
}

/// A debuggable program found by project detection.
#[derive(Debug, Clone)]
pub struct Program {
    pub path: PathBuf,
    /// Path relative to the working directory, for display and filtering.
    pub label: String,
}

impl Program {
    pub fn new(path: PathBuf, cwd: &Path) -> Self {
        let label = path
            .strip_prefix(cwd)
            .unwrap_or(&path)
            .display()
            .to_string();
        Self { path, label }
    }
}

impl PickerItem for Program {
    fn label(&self) -> &str {
        &self.label
    }
}

pub type TestPicker = Picker<TestCase>;
pub type ProgramPicker = Picker<Program>;
/// Source files, listed relative to the working directory like programs.
pub type FilePicker = Picker<Program>;
pub type FunctionPicker = Picker<crate::functions::Function>;

pub struct Picker<T> {
    /// `None` while discovery is in progress.
    pub items: Option<Vec<T>>,
    pub filter: String,
    /// Index into [`Self::matches`].
    pub cursor: usize,
}

impl<T> Default for Picker<T> {
    fn default() -> Self {
        Self {
            items: None,
            filter: String::new(),
            cursor: 0,
        }
    }
}

impl<T: PickerItem> Picker<T> {
    pub fn with_items(items: Vec<T>) -> Self {
        let mut p = Self::default();
        p.set_items(items);
        p
    }

    /// Items matching the filter: every whitespace-separated word must
    /// appear (case-insensitively) in the label.
    pub fn matches(&self) -> Vec<&T> {
        let words: Vec<String> = self
            .filter
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        self.items
            .iter()
            .flatten()
            .filter(|t| {
                let label = t.label().to_lowercase();
                words.iter().all(|w| label.contains(w.as_str()))
            })
            .collect()
    }

    pub fn selected(&self) -> Option<&T> {
        self.matches().get(self.cursor).copied()
    }

    pub fn set_items(&mut self, items: Vec<T>) {
        self.items = Some(items);
        self.clamp();
    }

    pub fn move_cursor(&mut self, delta: i64) {
        let max = self.matches().len().saturating_sub(1) as i64;
        self.cursor = (self.cursor as i64 + delta).clamp(0, max) as usize;
    }

    pub fn push(&mut self, c: char) {
        self.filter.push(c);
        self.clamp();
    }

    pub fn pop(&mut self) {
        self.filter.pop();
        self.clamp();
    }

    fn clamp(&mut self) {
        self.cursor = self.cursor.min(self.matches().len().saturating_sub(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddbg_test::{ProviderData, ProviderId, TestId};

    fn case(name: &str) -> TestCase {
        TestCase {
            id: TestId {
                provider: ProviderId::Rust,
                name: name.into(),
                data: ProviderData::Rust {
                    binary: "/b".into(),
                    package: "p".into(),
                    target: "p".into(),
                    package_dir: "/p".into(),
                },
            },
            name: name.into(),
            display_name: name.into(),
            source: None,
            line: None,
            suite: None,
        }
    }

    fn picker() -> TestPicker {
        Picker::with_items(vec![
            case("parser::tests::empty"),
            case("parser::tests::nested"),
            case("lexer::tests::empty"),
        ])
    }

    #[test]
    fn filters_by_all_words() {
        let mut p = picker();
        assert_eq!(p.matches().len(), 3);
        "Empty".chars().for_each(|c| p.push(c));
        assert_eq!(p.matches().len(), 2);
        " lex".chars().for_each(|c| p.push(c));
        assert_eq!(p.selected().unwrap().name, "lexer::tests::empty");
    }

    #[test]
    fn cursor_stays_in_range() {
        let mut p = picker();
        p.move_cursor(10);
        assert_eq!(p.cursor, 2);
        "nested".chars().for_each(|c| p.push(c));
        assert_eq!(p.cursor, 0);
        assert_eq!(p.selected().unwrap().name, "parser::tests::nested");
        "zzz".chars().for_each(|c| p.push(c));
        assert!(p.selected().is_none());
        p.move_cursor(-1);
        assert_eq!(p.cursor, 0);
    }

    #[test]
    fn empty_while_loading() {
        let p = TestPicker::default();
        assert!(p.matches().is_empty());
        assert!(p.selected().is_none());
    }

    #[test]
    fn programs_filter_on_relative_path() {
        let cwd = Path::new("/w");
        let mut p = ProgramPicker::with_items(vec![
            Program::new("/w/target/debug/server".into(), cwd),
            Program::new("/w/target/debug/client".into(), cwd),
        ]);
        assert_eq!(p.selected().unwrap().label, "target/debug/server");
        "cli".chars().for_each(|c| p.push(c));
        assert_eq!(
            p.selected().unwrap().path,
            Path::new("/w/target/debug/client")
        );
    }
}
