use super::stack::Stack;

/// An operation that was performed on the stack, for display in the undo list.
#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub description: String,
    pub snapshot: Stack,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

/// Maintains an undo/redo history as a timeline of stack states.
///
/// entries[0] is the initial state, entries[cursor] is the current state.
/// Undo decrements cursor, redo increments it.
pub struct History {
    entries: Vec<HistoryEntry>,
    /// Index of the current state in entries.
    cursor: usize,
    max_entries: usize,
}

impl History {
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: Vec::new(),
            cursor: 0,
            max_entries,
        }
    }

    /// Record a new state after an operation.
    /// Call this AFTER modifying the stack — pass the resulting state.
    pub fn push(&mut self, description: impl Into<String>, stack: &Stack) {
        // If cursor isn't at the end, discard redo-able future
        if !self.entries.is_empty() {
            self.entries.truncate(self.cursor + 1);
        }
        self.entries.push(HistoryEntry {
            description: description.into(),
            snapshot: stack.clone(),
            timestamp: chrono::Utc::now(),
        });
        self.cursor = self.entries.len() - 1;

        // Enforce max entries by evicting from the front
        if self.entries.len() > self.max_entries {
            self.entries.remove(0);
            self.cursor = self.entries.len() - 1;
        }
    }

    /// Undo: move to the previous state. Returns None if already at the oldest.
    pub fn undo(&mut self) -> Option<&Stack> {
        if self.cursor == 0 || self.entries.is_empty() {
            return None;
        }
        self.cursor -= 1;
        Some(&self.entries[self.cursor].snapshot)
    }

    /// Redo: move to the next state. Returns None if already at the newest.
    pub fn redo(&mut self) -> Option<&Stack> {
        if self.entries.is_empty() || self.cursor >= self.entries.len() - 1 {
            return None;
        }
        self.cursor += 1;
        Some(&self.entries[self.cursor].snapshot)
    }

    /// List all history entries for display.
    pub fn list(&self) -> &[HistoryEntry] {
        &self.entries
    }

    /// The current position (cursor index) in the timeline.
    pub fn position(&self) -> usize {
        self.cursor
    }

    pub fn total(&self) -> usize {
        self.entries.len()
    }

    pub fn can_undo(&self) -> bool {
        !self.entries.is_empty() && self.cursor > 0
    }

    pub fn can_redo(&self) -> bool {
        !self.entries.is_empty() && self.cursor < self.entries.len() - 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::stack::PatchEntry;

    fn make_stack(n: usize) -> Stack {
        let patches: Vec<PatchEntry> = (0..n)
            .map(|i| PatchEntry::new(&format!("h{}", i), &format!("c{}", i)))
            .collect();
        Stack::new("main".into(), patches)
    }

    #[test]
    fn test_empty_history() {
        let h = History::new(10);
        assert_eq!(h.total(), 0);
        assert!(!h.can_undo());
        assert!(!h.can_redo());
    }

    #[test]
    fn test_push_stores_state() {
        let mut h = History::new(10);
        h.push("initial", &make_stack(3));
        assert_eq!(h.total(), 1);
        assert_eq!(h.position(), 0);
        assert!(!h.can_undo());
        assert!(!h.can_redo());
    }

    #[test]
    fn test_undo_and_redo_basic() {
        let mut h = History::new(10);
        h.push("initial", &make_stack(3));
        h.push("after action1", &make_stack(2));
        h.push("after action2", &make_stack(1));
        assert_eq!(h.position(), 2);
        assert!(h.can_undo());
        assert!(!h.can_redo());

        // Undo action2 → back to state after action1
        let s = h.undo().unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(h.position(), 1);
        assert!(h.can_undo());
        assert!(h.can_redo());

        // Undo action1 → back to initial state
        let s = h.undo().unwrap();
        assert_eq!(s.len(), 3);
        assert_eq!(h.position(), 0);
        assert!(!h.can_undo());
        assert!(h.can_redo());

        // Can't undo past the beginning
        assert!(h.undo().is_none());

        // Redo → state after action1
        let s = h.redo().unwrap();
        assert_eq!(s.len(), 2);

        // Redo → state after action2
        let s = h.redo().unwrap();
        assert_eq!(s.len(), 1);

        // Can't redo past the end
        assert!(h.redo().is_none());
    }

    #[test]
    fn test_push_after_undo_discards_redo() {
        let mut h = History::new(10);
        h.push("initial", &make_stack(3));
        h.push("action1", &make_stack(2));
        h.push("action2", &make_stack(1));

        // Undo twice → at initial
        h.undo();
        h.undo();
        assert_eq!(h.position(), 0);

        // New action → discards action1 and action2
        h.push("action3", &make_stack(5));
        assert_eq!(h.total(), 2); // initial + action3
        assert_eq!(h.position(), 1);
        assert!(!h.can_redo());
        assert_eq!(h.list()[1].snapshot.len(), 5);
    }

    #[test]
    fn test_max_entries_enforced() {
        let mut h = History::new(3);
        h.push("a", &make_stack(1));
        h.push("b", &make_stack(2));
        h.push("c", &make_stack(3));
        assert_eq!(h.total(), 3);

        // Pushing a 4th should evict the oldest
        h.push("d", &make_stack(4));
        assert_eq!(h.total(), 3);
        assert_eq!(h.list()[0].description, "b");
        assert_eq!(h.list()[2].description, "d");
    }

    #[test]
    fn test_list_returns_all_entries() {
        let mut h = History::new(10);
        h.push("a", &make_stack(1));
        h.push("b", &make_stack(2));
        let entries = h.list();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].description, "a");
        assert_eq!(entries[1].description, "b");
    }

    #[test]
    fn test_undo_redo_roundtrip() {
        let mut h = History::new(10);
        let s1 = make_stack(3);
        let s2 = make_stack(1);
        h.push("initial", &s1);
        h.push("dropped", &s2);

        // Undo → initial state
        let undone = h.undo().unwrap();
        assert_eq!(*undone, s1);

        // Redo → dropped state
        let redone = h.redo().unwrap();
        assert_eq!(*redone, s2);
    }
}
