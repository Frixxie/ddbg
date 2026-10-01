//! Test picker: a filterable list of discovered tests.

use ddbg_cli::TestCase;

#[derive(Default)]
pub struct TestPicker {
    /// `None` while discovery is in progress.
    pub tests: Option<Vec<TestCase>>,
    pub filter: String,
    /// Index into [`Self::matches`].
    pub cursor: usize,
}

impl TestPicker {
    /// Tests matching the filter: every whitespace-separated word must
    /// appear (case-insensitively) in the test name.
    pub fn matches(&self) -> Vec<&TestCase> {
        let words: Vec<String> = self
            .filter
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        self.tests
            .iter()
            .flatten()
            .filter(|t| {
                let name = t.name.to_lowercase();
                words.iter().all(|w| name.contains(w.as_str()))
            })
            .collect()
    }

    pub fn selected(&self) -> Option<&TestCase> {
        self.matches().get(self.cursor).copied()
    }

    pub fn set_tests(&mut self, tests: Vec<TestCase>) {
        self.tests = Some(tests);
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
        let mut p = TestPicker::default();
        p.set_tests(vec![
            case("parser::tests::empty"),
            case("parser::tests::nested"),
            case("lexer::tests::empty"),
        ]);
        p
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
}
