use serde::{Deserialize, Serialize};

/// A single commit entry in the stack.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PatchEntry {
    pub hash: String,
    pub subject: String,
    pub body: String,
    pub author: String,
    pub timestamp: String,
    /// If a PR has been created for this commit
    pub pr_number: Option<u64>,
    /// Current status in the stack
    pub status: PatchStatus,
}

impl PatchEntry {
    /// Create a minimal entry (useful for testing and placeholders).
    pub fn new(hash: &str, subject: &str) -> Self {
        Self {
            hash: hash.to_string(),
            subject: subject.to_string(),
            body: String::new(),
            author: String::new(),
            timestamp: String::new(),
            pr_number: None,
            status: PatchStatus::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub enum PatchStatus {
    #[default]
    Clean,
    Conflict,
    Editing,
    Submitted,
    Merged,
}

/// The full stack state — an ordered list of patches from bottom (oldest) to top (newest).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stack {
    /// Base branch this stack is built on (e.g. "main", "origin/main")
    pub base: String,
    /// Ordered patches, index 0 = bottom of stack (oldest)
    pub patches: Vec<PatchEntry>,
}

impl Stack {
    pub fn new(base: String, patches: Vec<PatchEntry>) -> Self {
        Self { base, patches }
    }

    pub fn len(&self) -> usize {
        self.patches.len()
    }

    pub fn is_empty(&self) -> bool {
        self.patches.is_empty()
    }

    /// Squash patches at `indices` into the lowest-indexed one.
    pub fn squash(&mut self, indices: &[usize]) -> Result<(), StackError> {
        if indices.is_empty() {
            return Err(StackError::EmptySelection);
        }
        let mut sorted: Vec<usize> = indices.to_vec();
        sorted.sort();
        sorted.dedup();

        // Single commit — nothing to squash
        if sorted.len() == 1 {
            return Ok(());
        }
        if *sorted.last().unwrap() >= self.patches.len() {
            return Err(StackError::OutOfBounds);
        }

        let target_idx = sorted[0];
        let squashed_subjects: Vec<String> = sorted
            .iter()
            .skip(1)
            .map(|&i| self.patches[i].subject.clone())
            .collect();

        // Merge subjects into the target's body
        let addendum = squashed_subjects.join("\n");
        if self.patches[target_idx].body.is_empty() {
            self.patches[target_idx].body = addendum;
        } else {
            self.patches[target_idx].body.push('\n');
            self.patches[target_idx].body.push_str(&addendum);
        }
        self.patches[target_idx].subject = format!(
            "{} (+{} squashed)",
            self.patches[target_idx].subject,
            squashed_subjects.len()
        );

        // Remove squashed entries highest-index-first to keep indices stable
        for &i in sorted.iter().skip(1).rev() {
            self.patches.remove(i);
        }
        Ok(())
    }

    /// Move a patch from `from` to `to` position.
    pub fn reorder(&mut self, from: usize, to: usize) -> Result<(), StackError> {
        if from >= self.patches.len() || to >= self.patches.len() {
            return Err(StackError::OutOfBounds);
        }
        if from == to {
            return Ok(());
        }
        let entry = self.patches.remove(from);
        self.patches.insert(to, entry);
        Ok(())
    }

    /// Insert a new patch at the given position.
    pub fn insert_at(&mut self, index: usize, entry: PatchEntry) -> Result<(), StackError> {
        if index > self.patches.len() {
            return Err(StackError::OutOfBounds);
        }
        self.patches.insert(index, entry);
        Ok(())
    }

    /// Drop a patch from the stack.
    pub fn drop_patch(&mut self, index: usize) -> Result<PatchEntry, StackError> {
        if index >= self.patches.len() {
            return Err(StackError::OutOfBounds);
        }
        Ok(self.patches.remove(index))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StackError {
    #[error("no patches selected")]
    EmptySelection,
    #[error("index out of bounds")]
    OutOfBounds,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_stack(n: usize) -> Stack {
        let patches: Vec<PatchEntry> = (0..n)
            .map(|i| PatchEntry::new(&format!("abc{:05}", i), &format!("commit {}", i)))
            .collect();
        Stack::new("origin/main".into(), patches)
    }

    // --- basic ---

    #[test]
    fn test_stack_len_and_empty() {
        assert!(make_stack(0).is_empty());
        assert_eq!(make_stack(0).len(), 0);
        assert_eq!(make_stack(5).len(), 5);
        assert!(!make_stack(1).is_empty());
    }

    // --- squash ---

    #[test]
    fn test_squash_two_adjacent() {
        let mut s = make_stack(4);
        s.squash(&[1, 2]).unwrap();
        assert_eq!(s.len(), 3);
        assert!(s.patches[1].subject.contains("+1 squashed"));
        assert!(s.patches[1].body.contains("commit 2"));
        assert_eq!(s.patches[2].subject, "commit 3");
    }

    #[test]
    fn test_squash_three() {
        let mut s = make_stack(5);
        s.squash(&[0, 1, 2]).unwrap();
        assert_eq!(s.len(), 3);
        assert!(s.patches[0].subject.contains("+2 squashed"));
        assert!(s.patches[0].body.contains("commit 1"));
        assert!(s.patches[0].body.contains("commit 2"));
    }

    #[test]
    fn test_squash_non_contiguous() {
        let mut s = make_stack(5);
        s.squash(&[0, 2, 4]).unwrap();
        assert_eq!(s.len(), 3);
        assert!(s.patches[0].subject.contains("+2 squashed"));
        assert_eq!(s.patches[1].subject, "commit 1");
        assert_eq!(s.patches[2].subject, "commit 3");
    }

    #[test]
    fn test_squash_single_noop() {
        let mut s = make_stack(3);
        let before = s.clone();
        s.squash(&[1]).unwrap();
        assert_eq!(s, before);
    }

    #[test]
    fn test_squash_empty_err() {
        let mut s = make_stack(3);
        assert!(s.squash(&[]).is_err());
    }

    #[test]
    fn test_squash_out_of_bounds() {
        let mut s = make_stack(3);
        assert!(s.squash(&[1, 5]).is_err());
    }

    #[test]
    fn test_squash_dedup_indices() {
        let mut s = make_stack(4);
        s.squash(&[1, 2, 2, 1]).unwrap();
        assert_eq!(s.len(), 3);
    }

    #[test]
    fn test_squash_preserves_existing_body() {
        let mut s = make_stack(3);
        s.patches[0].body = "existing body".into();
        s.squash(&[0, 1]).unwrap();
        assert!(s.patches[0].body.starts_with("existing body\n"));
        assert!(s.patches[0].body.contains("commit 1"));
    }

    // --- reorder ---

    #[test]
    fn test_reorder_adjacent() {
        let mut s = make_stack(3);
        s.reorder(0, 1).unwrap();
        assert_eq!(s.patches[0].subject, "commit 1");
        assert_eq!(s.patches[1].subject, "commit 0");
    }

    #[test]
    fn test_reorder_to_end() {
        let mut s = make_stack(4);
        s.reorder(0, 3).unwrap();
        assert_eq!(s.patches[3].subject, "commit 0");
    }

    #[test]
    fn test_reorder_same_pos_noop() {
        let mut s = make_stack(3);
        let before = s.clone();
        s.reorder(1, 1).unwrap();
        assert_eq!(s, before);
    }

    #[test]
    fn test_reorder_out_of_bounds() {
        let mut s = make_stack(3);
        assert!(s.reorder(0, 5).is_err());
        assert!(s.reorder(5, 0).is_err());
    }

    // --- insert ---

    #[test]
    fn test_insert_at_beginning() {
        let mut s = make_stack(2);
        s.insert_at(0, PatchEntry::new("new", "inserted")).unwrap();
        assert_eq!(s.len(), 3);
        assert_eq!(s.patches[0].subject, "inserted");
    }

    #[test]
    fn test_insert_at_end() {
        let mut s = make_stack(2);
        s.insert_at(2, PatchEntry::new("new", "appended")).unwrap();
        assert_eq!(s.len(), 3);
        assert_eq!(s.patches[2].subject, "appended");
    }

    #[test]
    fn test_insert_out_of_bounds() {
        let mut s = make_stack(2);
        assert!(s.insert_at(5, PatchEntry::new("x", "x")).is_err());
    }

    // --- drop ---

    #[test]
    fn test_drop_middle() {
        let mut s = make_stack(4);
        let dropped = s.drop_patch(2).unwrap();
        assert_eq!(dropped.subject, "commit 2");
        assert_eq!(s.len(), 3);
    }

    #[test]
    fn test_drop_last_remaining() {
        let mut s = make_stack(1);
        s.drop_patch(0).unwrap();
        assert!(s.is_empty());
    }

    #[test]
    fn test_drop_out_of_bounds() {
        let mut s = make_stack(2);
        assert!(s.drop_patch(5).is_err());
    }

    // --- PatchEntry ---

    #[test]
    fn test_patch_entry_defaults() {
        let p = PatchEntry::new("abc", "test");
        assert_eq!(p.status, PatchStatus::Clean);
        assert_eq!(p.pr_number, None);
        assert!(p.body.is_empty());
    }
}
