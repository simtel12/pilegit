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

