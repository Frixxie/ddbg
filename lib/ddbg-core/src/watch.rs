//! Persistent expressions and their current stopped-state summaries.

use std::collections::BTreeMap;
use std::fmt;

use crate::variable::Evaluation;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WatchId(pub u32);

impl fmt::Display for WatchId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A top-level summary only. No adapter variable references are retained;
/// use `print` on the expression to inspect its children on demand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchValue {
    pub value: String,
    pub type_name: Option<String>,
    pub has_children: bool,
}

impl From<Evaluation> for WatchValue {
    fn from(e: Evaluation) -> Self {
        Self {
            value: e.value,
            type_name: e.type_name,
            has_children: e.children.is_some(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Watch {
    pub id: WatchId,
    pub expression: String,
    /// `None` until evaluated, or while the program is not stopped.
    pub result: Option<Result<WatchValue, String>>,
}

#[derive(Debug, Default)]
pub struct WatchStore {
    next_id: u32,
    items: BTreeMap<WatchId, Watch>,
}

impl WatchStore {
    /// The engine validates expressions before adding them. Duplicates keep
    /// their id; expressions remain configured across program restarts.
    pub fn add(&mut self, expression: String) -> (WatchId, bool) {
        if let Some(w) = self.items.values().find(|w| w.expression == expression) {
            return (w.id, false);
        }
        self.next_id += 1;
        let id = WatchId(self.next_id);
        self.items.insert(
            id,
            Watch {
                id,
                expression,
                result: None,
            },
        );
        (id, true)
    }

    pub fn get(&self, id: WatchId) -> Option<&Watch> {
        self.items.get(&id)
    }

    pub fn remove(&mut self, id: WatchId) -> Option<Watch> {
        self.items.remove(&id)
    }

    pub fn snapshot(&self) -> Vec<Watch> {
        self.items.values().cloned().collect()
    }

    pub fn set_result(&mut self, id: WatchId, result: Result<WatchValue, String>) {
        if let Some(w) = self.items.get_mut(&id) {
            w.result = Some(result);
        }
    }

    pub fn invalidate(&mut self) {
        for w in self.items.values_mut() {
            w.result = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quickcheck_macros::quickcheck;

    #[quickcheck]
    fn operations_match_reference_model(operations: Vec<(u8, String, u8)>) -> bool {
        let mut store = WatchStore::default();
        let mut model = BTreeMap::<WatchId, Watch>::new();
        let mut next_id = 0;
        for (operation, text, selected) in operations {
            // Target allocated IDs as well as unknown/removed ones, rather
            // than spending most generated operations on nonexistent watches.
            let id = WatchId(u32::from(selected) % (next_id + 2));
            match operation % 4 {
                0 => {
                    let expected = match model.values().find(|w| w.expression == text) {
                        Some(watch) => (watch.id, false),
                        None => {
                            next_id += 1;
                            let id = WatchId(next_id);
                            model.insert(
                                id,
                                Watch {
                                    id,
                                    expression: text.clone(),
                                    result: None,
                                },
                            );
                            (id, true)
                        }
                    };
                    assert_eq!(store.add(text), expected);
                }
                1 => assert_eq!(store.remove(id), model.remove(&id)),
                2 => {
                    let result = if selected.is_multiple_of(2) {
                        Ok(WatchValue {
                            value: text,
                            type_name: None,
                            has_children: false,
                        })
                    } else {
                        Err(text)
                    };
                    store.set_result(id, result.clone());
                    if let Some(watch) = model.get_mut(&id) {
                        watch.result = Some(result);
                    }
                }
                _ => {
                    store.invalidate();
                    for watch in model.values_mut() {
                        watch.result = None;
                    }
                }
            }
            assert_eq!(
                store.snapshot(),
                model.values().cloned().collect::<Vec<_>>()
            );
            assert_eq!(store.get(id), model.get(&id));
        }
        true
    }

    #[test]
    fn duplicates_keep_ids_and_invalidation_keeps_expressions() {
        let mut s = WatchStore::default();
        let (id, new) = s.add("x + 1".into());
        assert!(new);
        assert_eq!(s.add("x + 1".into()), (id, false));
        s.set_result(
            id,
            Ok(WatchValue {
                value: "42".into(),
                type_name: None,
                has_children: false,
            }),
        );
        s.invalidate();
        assert_eq!(s.get(id).unwrap().expression, "x + 1");
        assert_eq!(s.get(id).unwrap().result, None);
        s.remove(id).unwrap();
        assert!(s.snapshot().is_empty());
        assert!(s.add("x".into()).0 > id);
    }
}
