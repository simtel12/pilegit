use color_eyre::Result;
use crossterm::event::{self, Event, KeyEvent};
use std::time::Duration;

use super::input;
use super::ui;
use super::Tui;
use crate::core::history::History;
use crate::core::stack::Stack;

const HELP_MSG: &str =
    "↑/k ↓/j: move | V: select | s: squash | K/J: reorder | d: diff | i: insert | R: rebase | S: submit | u: undo | q: quit";

/// Why the TUI is being suspended.
#[derive(Debug, Clone, PartialEq)]
pub enum SuspendReason {
    /// User pressed 'i' to insert a new commit
    InsertCommit,
    /// Rebase has conflicts that need manual resolution
    RebaseConflict,
}

/// Interaction mode for the TUI.
#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    /// Normal navigation
    Normal,
    /// Visual selection — j/k to extend selection
    Select,
    /// Viewing diff of a commit
    DiffView,
    /// Viewing undo history
    HistoryView,
    /// Confirm dialog (e.g. before squash)
    Confirm {
        prompt: String,
        action: PendingAction,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum PendingAction {
    Squash,
    Drop,
    Rebase,
}

pub struct App {
    pub stack: Stack,
    pub history: History,
    pub mode: Mode,
    /// Cursor position in the patch list (0 = oldest, len-1 = newest/top)
    pub cursor: usize,
    /// Selection anchor for visual mode (inclusive range anchor..cursor)
    pub select_anchor: Option<usize>,
    /// Currently expanded commit (showing details)
    pub expanded: Option<usize>,
    /// Scroll offset for the list view
    pub scroll_offset: usize,
    /// Diff content when in DiffView mode
    pub diff_content: Vec<String>,
    pub diff_scroll: usize,
    /// Status bar message
    pub status_msg: String,
    /// Should quit
    pub should_quit: bool,
    /// Set when the TUI should suspend for user shell interaction
    pub wants_suspend: Option<SuspendReason>,
    /// Custom command template for submitting PRs/CLs (e.g. "arc diff HEAD^")
    pub submit_cmd: Option<String>,
}

impl App {
    pub fn new(stack: Stack) -> Self {
        let cursor = if stack.is_empty() {
            0
        } else {
            stack.len() - 1
        };
        let mut history = History::new(100);
        history.push("initial", &stack);

        // Read submit command from environment
        let submit_cmd = std::env::var("PGIT_SUBMIT_CMD").ok();

        Self {
            stack,
            history,
            mode: Mode::Normal,
            cursor,
            select_anchor: None,
            expanded: None,
            scroll_offset: 0,
            diff_content: Vec::new(),
            diff_scroll: 0,
            status_msg: HELP_MSG.to_string(),
            should_quit: false,
            wants_suspend: None,
            submit_cmd,
        }
    }

    /// Main event loop.
    pub fn run(&mut self, terminal: &mut Tui) -> Result<()> {
        while !self.should_quit {
            // Check if we need to suspend for shell interaction
            if self.wants_suspend.is_some() {
                return Ok(());
            }

            terminal.draw(|frame| ui::render(frame, self))?;

            if event::poll(Duration::from_millis(100))? {
                if let Event::Key(key) = event::read()? {
                    self.handle_key(key);
                }
            }
        }
        Ok(())
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        match &self.mode {
            Mode::Normal => input::handle_normal(self, key),
            Mode::Select => input::handle_select(self, key),
            Mode::DiffView => input::handle_diff_view(self, key),
            Mode::HistoryView => input::handle_history_view(self, key),
            Mode::Confirm { .. } => input::handle_confirm(self, key),
        }
    }

    /// Get the selected range (inclusive), always ordered low..=high.
    pub fn selection_range(&self) -> Option<(usize, usize)> {
        self.select_anchor.map(|anchor| {
            let lo = anchor.min(self.cursor);
            let hi = anchor.max(self.cursor);
            (lo, hi)
        })
    }

    /// Record current state after an operation for undo/redo.
    fn record(&mut self, description: &str) {
        self.history.push(description, &self.stack);
    }

    pub fn undo(&mut self) {
        if let Some(prev) = self.history.undo() {
            self.stack = prev.clone();
            self.clamp_cursor();
            self.status_msg = format!(
                "Undone. ({}/{})",
                self.history.position(),
                self.history.total()
            );
        } else {
            self.status_msg = "Nothing to undo.".into();
        }
    }

    pub fn redo(&mut self) {
        if let Some(next) = self.history.redo() {
            self.stack = next.clone();
            self.clamp_cursor();
            self.status_msg = format!(
                "Redone. ({}/{})",
                self.history.position(),
                self.history.total()
            );
        } else {
            self.status_msg = "Nothing to redo.".into();
        }
    }

    pub fn clamp_cursor(&mut self) {
        if self.stack.is_empty() {
            self.cursor = 0;
        } else if self.cursor >= self.stack.len() {
            self.cursor = self.stack.len() - 1;
        }
    }

    // ---------------------------------------------------------------
    // Cursor movement: "up" = visually upward = toward newer = higher index
    //                  "down" = visually downward = toward older = lower index
    // The display renders index (len-1) at the top, index 0 at the bottom.
    // ---------------------------------------------------------------

    /// Move cursor visually upward (toward newer commits = higher index).
    pub fn move_cursor_up(&mut self) {
        if !self.stack.is_empty() && self.cursor < self.stack.len() - 1 {
            self.cursor += 1;
        }
    }

    /// Move cursor visually downward (toward older commits = lower index).
    pub fn move_cursor_down(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
        }
    }

    /// Move the patch at cursor one position visually upward (swap with next).
    pub fn move_patch_up(&mut self) {
        if !self.stack.is_empty() && self.cursor < self.stack.len() - 1 {
            let _ = self.stack.reorder(self.cursor, self.cursor + 1);
            self.cursor += 1;
            self.record("reorder patch up");
            self.status_msg = "Patch moved up.".into();
        }
    }

    /// Move the patch at cursor one position visually downward (swap with prev).
    pub fn move_patch_down(&mut self) {
        if self.cursor > 0 && !self.stack.is_empty() {
            let _ = self.stack.reorder(self.cursor, self.cursor - 1);
            self.cursor -= 1;
            self.record("reorder patch down");
            self.status_msg = "Patch moved down.".into();
        }
    }

    pub fn squash_selected(&mut self) {
        if let Some((lo, hi)) = self.selection_range() {
            let indices: Vec<usize> = (lo..=hi).collect();
            let count = indices.len();
            match self.stack.squash(&indices) {
                Ok(()) => {
                    self.record("squash commits");
                    self.select_anchor = None;
                    self.mode = Mode::Normal;
                    self.cursor = lo;
                    self.clamp_cursor();
                    self.status_msg = format!("Squashed {} commits.", count);
                }
                Err(e) => {
                    self.status_msg = format!("Squash failed: {}", e);
                }
            }
        } else {
            self.status_msg = "No selection. Use V or Shift+arrows to select.".into();
        }
    }

    pub fn drop_at_cursor(&mut self) {
        if self.stack.is_empty() {
            return;
        }
        match self.stack.drop_patch(self.cursor) {
            Ok(dropped) => {
                self.record("drop commit");
                self.clamp_cursor();
                self.status_msg = format!("Dropped: {}", dropped.subject);
            }
            Err(e) => {
                self.status_msg = format!("Drop failed: {}", e);
            }
        }
    }

    /// Request insert mode — signals the run loop to suspend the TUI.
    pub fn insert_commit(&mut self) {
        self.wants_suspend = Some(SuspendReason::InsertCommit);
    }

    /// Reload the stack from git after an external operation (insert, rebase).
    pub fn reload_stack(&mut self) -> Result<()> {
        let repo = crate::git::ops::Repo::open()?;
        let commits = repo.list_stack_commits()?;
        self.stack = Stack::new(self.stack.base.clone(), commits);
        self.record("reload");
        self.clamp_cursor();
        Ok(())
    }

    /// Start a rebase onto the base branch.
    pub fn start_rebase(&mut self) {
        self.mode = Mode::Confirm {
            prompt: format!("Rebase onto {}? (y/n)", self.stack.base),
            action: PendingAction::Rebase,
        };
    }

    /// Execute the rebase. Returns Ok(true) if clean, Ok(false) if conflicts.
    pub fn execute_rebase(&mut self) -> Result<bool> {
        let repo = crate::git::ops::Repo::open()?;
        let clean = repo.rebase_onto_base()?;
        if clean {
            self.reload_stack()?;
            self.status_msg = "Rebase completed successfully.".into();
            Ok(true)
        } else {
            // Show conflict info
            let conflicts = repo.conflicted_files().unwrap_or_default();
            self.status_msg = format!(
                "CONFLICT in {} file(s): {}. Resolve, stage, then press 'c' to continue or 'a' to abort.",
                conflicts.len(),
                conflicts.join(", ")
            );
            self.wants_suspend = Some(SuspendReason::RebaseConflict);
            Ok(false)
        }
    }

    /// Continue a rebase after conflict resolution.
    pub fn continue_rebase(&mut self) -> Result<bool> {
        let repo = crate::git::ops::Repo::open()?;
        let clean = repo.rebase_continue()?;
        if clean {
            self.reload_stack()?;
            self.status_msg = "Rebase completed successfully.".into();
            Ok(true)
        } else {
            let conflicts = repo.conflicted_files().unwrap_or_default();
            self.status_msg = format!(
                "CONFLICT in {} file(s): {}. Resolve, stage, then press 'c' to continue or 'a' to abort.",
                conflicts.len(),
                conflicts.join(", ")
            );
            self.wants_suspend = Some(SuspendReason::RebaseConflict);
            Ok(false)
        }
    }

    /// Abort a rebase in progress.
    pub fn abort_rebase(&mut self) -> Result<()> {
        let repo = crate::git::ops::Repo::open()?;
        repo.rebase_abort()?;
        self.reload_stack()?;
        self.status_msg = "Rebase aborted.".into();
        Ok(())
    }

    /// Submit the commit at cursor using the configured command.
    pub fn submit_at_cursor(&mut self) {
        let cmd = match &self.submit_cmd {
            Some(c) => c.clone(),
            None => {
                self.status_msg =
                    "No submit command configured. Set PGIT_SUBMIT_CMD env var (e.g. \"arc diff HEAD^\")."
                        .into();
                return;
            }
        };

        if self.stack.is_empty() {
            return;
        }

        let patch = &self.stack.patches[self.cursor];
        let hash = patch.hash.clone();
        let subject = patch.subject.clone();

        match crate::git::ops::Repo::open().and_then(|r| r.run_submit_cmd(&cmd, &hash, &subject)) {
            Ok(output) => {
                self.status_msg = format!("Submitted: {}", output.trim());
            }
            Err(e) => {
                self.status_msg = format!("Submit failed: {}", e);
            }
        }
    }

    pub fn reset_status(&mut self) {
        self.status_msg = HELP_MSG.to_string();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::stack::PatchEntry;
    use crossterm::event::{KeyCode, KeyModifiers};

    fn make_app(n: usize) -> App {
        let patches: Vec<PatchEntry> = (0..n)
            .map(|i| PatchEntry::new(&format!("h{:07}", i), &format!("commit {}", i)))
            .collect();
        App::new(Stack::new("main".into(), patches))
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn key_shift(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::SHIFT)
    }

    fn key_ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    // --- cursor direction ---
    // Display: newest (high index) at top, oldest (low index) at bottom
    // Up = toward newer = increment index
    // Down = toward older = decrement index

    #[test]
    fn test_initial_cursor_at_top() {
        let app = make_app(5);
        assert_eq!(app.cursor, 4); // newest commit, top of screen
    }

    #[test]
    fn test_initial_cursor_empty_stack() {
        let app = make_app(0);
        assert_eq!(app.cursor, 0);
    }

    #[test]
    fn test_cursor_up_stops_at_top() {
        let mut app = make_app(3);
        app.cursor = 2; // already at top (newest)
        app.move_cursor_up();
        assert_eq!(app.cursor, 2); // can't go higher
    }

    #[test]
    fn test_cursor_down_stops_at_bottom() {
        let mut app = make_app(3);
        app.cursor = 0; // already at bottom (oldest)
        app.move_cursor_down();
        assert_eq!(app.cursor, 0); // can't go lower
    }

    #[test]
    fn test_cursor_navigation_via_keys() {
        let mut app = make_app(5);
        assert_eq!(app.cursor, 4); // starts at top

        // j = move down (visually) = decrement
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.cursor, 3);
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.cursor, 2);

        // k = move up (visually) = increment
        app.handle_key(key(KeyCode::Char('k')));
        assert_eq!(app.cursor, 3);
    }

    #[test]
    fn test_jump_g_to_top_G_to_bottom() {
        let mut app = make_app(5);
        // g = top of screen = newest = len-1
        app.cursor = 0;
        app.handle_key(key(KeyCode::Char('g')));
        assert_eq!(app.cursor, 4);

        // G = bottom of screen = oldest = 0
        app.handle_key(key(KeyCode::Char('G')));
        assert_eq!(app.cursor, 0);
    }

    // --- select mode ---

    #[test]
    fn test_enter_visual_select() {
        let mut app = make_app(5);
        app.cursor = 2;
        app.handle_key(key(KeyCode::Char('V')));
        assert_eq!(app.mode, Mode::Select);
        assert_eq!(app.select_anchor, Some(2));
    }

    #[test]
    fn test_select_and_cancel() {
        let mut app = make_app(5);
        app.cursor = 2;
        app.handle_key(key(KeyCode::Char('V')));
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.select_anchor, None);
    }

    #[test]
    fn test_selection_range_ordered() {
        let mut app = make_app(5);
        app.cursor = 3;
        app.select_anchor = Some(1);
        assert_eq!(app.selection_range(), Some((1, 3)));

        app.cursor = 1;
        app.select_anchor = Some(3);
        assert_eq!(app.selection_range(), Some((1, 3)));
    }

    #[test]
    fn test_select_extend_with_j_k() {
        let mut app = make_app(5);
        app.cursor = 3;
        app.handle_key(key(KeyCode::Char('V'))); // anchor at 3
        assert_eq!(app.mode, Mode::Select);

        // j = down visually = decrement cursor
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.cursor, 2);
        assert_eq!(app.selection_range(), Some((2, 3)));

        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.cursor, 1);
        assert_eq!(app.selection_range(), Some((1, 3)));

        // k = up visually = increment cursor
        app.handle_key(key(KeyCode::Char('k')));
        assert_eq!(app.cursor, 2);
        assert_eq!(app.selection_range(), Some((2, 3)));
    }

    #[test]
    fn test_shift_down_enters_select_and_moves_down() {
        let mut app = make_app(5);
        app.cursor = 2;
        app.handle_key(key_shift(KeyCode::Down));
        assert_eq!(app.mode, Mode::Select);
        assert_eq!(app.select_anchor, Some(2));
        assert_eq!(app.cursor, 1); // visual down = decrement
    }

    #[test]
    fn test_shift_up_enters_select_and_moves_up() {
        let mut app = make_app(5);
        app.cursor = 2;
        app.handle_key(key_shift(KeyCode::Up));
        assert_eq!(app.mode, Mode::Select);
        assert_eq!(app.select_anchor, Some(2));
        assert_eq!(app.cursor, 3); // visual up = increment
    }

    // --- squash ---

    #[test]
    fn test_squash_via_select() {
        let mut app = make_app(5);
        app.cursor = 2;
        app.select_anchor = Some(2);
        app.mode = Mode::Select;

        // Extend selection down visually (j = decrement)
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.cursor, 1);
        assert_eq!(app.selection_range(), Some((1, 2)));

        // Trigger squash confirm
        app.handle_key(key(KeyCode::Char('s')));
        assert!(matches!(app.mode, Mode::Confirm { .. }));

        // Confirm
        app.handle_key(key(KeyCode::Char('y')));
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.stack.len(), 4); // 5 - 1 squashed
    }

    // --- reorder ---

    #[test]
    fn test_reorder_patch_up() {
        // K moves patch visually up = swap with higher index
        let mut app = make_app(4);
        app.cursor = 2;
        app.handle_key(key(KeyCode::Char('K')));
        assert_eq!(app.cursor, 3); // moved up visually
        assert_eq!(app.stack.patches[3].subject, "commit 2"); // moved up
        assert_eq!(app.stack.patches[2].subject, "commit 3"); // swapped down
    }

    #[test]
    fn test_reorder_patch_down() {
        // J moves patch visually down = swap with lower index
        let mut app = make_app(4);
        app.cursor = 2;
        app.handle_key(key(KeyCode::Char('J')));
        assert_eq!(app.cursor, 1); // moved down visually
        assert_eq!(app.stack.patches[1].subject, "commit 2"); // moved down
        assert_eq!(app.stack.patches[2].subject, "commit 1"); // swapped up
    }

    // --- drop ---

    #[test]
    fn test_drop_with_confirm() {
        let mut app = make_app(3);
        app.cursor = 1;
        app.handle_key(key(KeyCode::Char('x')));
        assert!(matches!(app.mode, Mode::Confirm { .. }));

        app.handle_key(key(KeyCode::Char('y')));
        assert_eq!(app.stack.len(), 2);
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn test_drop_cancel() {
        let mut app = make_app(3);
        app.cursor = 1;
        app.handle_key(key(KeyCode::Char('x')));
        app.handle_key(key(KeyCode::Char('n')));
        assert_eq!(app.stack.len(), 3);
    }

    // --- undo/redo ---

    #[test]
    fn test_undo_redo() {
        let mut app = make_app(4);
        let original_len = app.stack.len();
        app.cursor = 1;

        // Drop a commit
        app.handle_key(key(KeyCode::Char('x')));
        app.handle_key(key(KeyCode::Char('y')));
        assert_eq!(app.stack.len(), 3);

        // Undo
        app.handle_key(key(KeyCode::Char('u')));
        assert_eq!(app.stack.len(), original_len);

        // Redo
        app.handle_key(key_ctrl(KeyCode::Char('r')));
        assert_eq!(app.stack.len(), 3);
    }

    #[test]
    fn test_undo_empty_history() {
        let mut app = make_app(3);
        app.handle_key(key(KeyCode::Char('u')));
        assert!(app.status_msg.contains("Nothing to undo"));
    }

    // --- clamp ---

    #[test]
    fn test_clamp_after_drop_last() {
        let mut app = make_app(3);
        app.cursor = 2;
        app.drop_at_cursor();
        assert!(app.cursor <= app.stack.len().saturating_sub(1));
    }

    #[test]
    fn test_clamp_on_empty() {
        let mut app = make_app(1);
        app.drop_at_cursor();
        assert_eq!(app.cursor, 0);
        assert!(app.stack.is_empty());
    }

    // --- other ---

    #[test]
    fn test_quit() {
        let mut app = make_app(3);
        app.handle_key(key(KeyCode::Char('q')));
        assert!(app.should_quit);
    }

    #[test]
    fn test_expand_collapse() {
        let mut app = make_app(3);
        app.cursor = 1;
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.expanded, Some(1));
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.expanded, None);
    }

    #[test]
    fn test_history_view_enter_and_exit() {
        let mut app = make_app(3);
        app.handle_key(key(KeyCode::Char('h')));
        assert_eq!(app.mode, Mode::HistoryView);
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn test_insert_sets_suspend() {
        let mut app = make_app(3);
        app.handle_key(key(KeyCode::Char('i')));
        assert_eq!(app.wants_suspend, Some(SuspendReason::InsertCommit));
    }

    #[test]
    fn test_rebase_triggers_confirm() {
        let mut app = make_app(3);
        app.handle_key(key(KeyCode::Char('R')));
        assert!(matches!(app.mode, Mode::Confirm { action: PendingAction::Rebase, .. }));
    }

    #[test]
    fn test_submit_without_config() {
        let mut app = make_app(3);
        app.submit_cmd = None;
        app.submit_at_cursor();
        assert!(app.status_msg.contains("No submit command configured"));
    }
}
