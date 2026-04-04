use color_eyre::Result;
use crossterm::event::{self, Event, KeyEvent};
use std::time::Duration;

use super::input;
use super::ui;
use super::Tui;
use crate::core::history::History;
use crate::core::stack::Stack;

const HELP_MSG: &str = "?: help";

const HELP_FULL: &str = "\
Navigation:  ↑/k up  ↓/j down  g top  G bottom  Enter expand
Select:      Shift+↑↓ start select  V select  j/k extend  s squash  Esc cancel
Reorder:     Alt+↑↓ or Alt+k/j move patch
Edit:        e edit commit  i insert at top  o insert above cursor
Stack:       R rebase  S submit  x remove  d diff
History:     u undo  Ctrl+r redo  h history view
             q quit  ? this help";

/// Why the TUI is being suspended.
#[derive(Debug, Clone, PartialEq)]
pub enum SuspendReason {
    /// User pressed 'i' to insert a new commit at HEAD
    InsertAtHead,
    /// User pressed 'o' to insert a commit above the cursor position
    InsertAboveCursor { hash: String },
    /// User pressed 'e' to edit/amend a specific commit
    EditCommit { hash: String },
    /// Rebase has conflicts that need manual resolution
    RebaseConflict,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    Normal,
    Select,
    DiffView,
    HistoryView,
    Help,
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
    /// Cursor position (0 = oldest/bottom, len-1 = newest/top)
    pub cursor: usize,
    pub select_anchor: Option<usize>,
    pub expanded: Option<usize>,
    pub scroll_offset: usize,
    pub diff_content: Vec<String>,
    pub diff_scroll: usize,
    pub status_msg: String,
    pub should_quit: bool,
    pub wants_suspend: Option<SuspendReason>,
    /// Custom command template for submitting PRs/CLs
    pub submit_cmd: Option<String>,
}

impl App {
    pub fn new(stack: Stack) -> Self {
        let cursor = if stack.is_empty() {
            0
        } else {
            stack.len() - 1
        };
        let mut history = History::new(500);
        history.push("initial", &stack);
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

    pub fn run(&mut self, terminal: &mut Tui) -> Result<()> {
        while !self.should_quit {
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
            Mode::Help => input::handle_help(self, key),
            Mode::Confirm { .. } => input::handle_confirm(self, key),
        }
    }

    pub fn selection_range(&self) -> Option<(usize, usize)> {
        self.select_anchor.map(|anchor| {
            let lo = anchor.min(self.cursor);
            let hi = anchor.max(self.cursor);
            (lo, hi)
        })
    }

    fn record(&mut self, description: &str) {
        self.history.push(description, &self.stack);
    }

    pub fn undo(&mut self) {
        if let Some(prev) = self.history.undo() {
            self.stack = prev.clone();
            self.clamp_cursor();
            self.set_status(format!(
                "Undone ({}/{})",
                self.history.position(),
                self.history.total()
            ));
        } else {
            self.set_status("Nothing to undo.");
        }
    }

    pub fn redo(&mut self) {
        if let Some(next) = self.history.redo() {
            self.stack = next.clone();
            self.clamp_cursor();
            self.set_status(format!(
                "Redone ({}/{})",
                self.history.position(),
                self.history.total()
            ));
        } else {
            self.set_status("Nothing to redo.");
        }
    }

    pub fn clamp_cursor(&mut self) {
        if self.stack.is_empty() {
            self.cursor = 0;
        } else if self.cursor >= self.stack.len() {
            self.cursor = self.stack.len() - 1;
        }
    }

    // -- Visual cursor: up = toward newer (higher index), down = toward older (lower index) --

    pub fn move_cursor_up(&mut self) {
        if !self.stack.is_empty() && self.cursor < self.stack.len() - 1 {
            self.cursor += 1;
        }
    }

    pub fn move_cursor_down(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
        }
    }

    /// Move patch at cursor visually upward (swap with higher index).
    pub fn move_patch_up(&mut self) {
        if !self.stack.is_empty() && self.cursor < self.stack.len() - 1 {
            let _ = self.stack.reorder(self.cursor, self.cursor + 1);
            self.cursor += 1;
            self.record("move patch up");
            self.set_status("Patch moved up.");
        }
    }

    /// Move patch at cursor visually downward (swap with lower index).
    pub fn move_patch_down(&mut self) {
        if self.cursor > 0 && !self.stack.is_empty() {
            let _ = self.stack.reorder(self.cursor, self.cursor - 1);
            self.cursor -= 1;
            self.record("move patch down");
            self.set_status("Patch moved down.");
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
                    self.set_status(format!("Squashed {} commits.", count));
                }
                Err(e) => self.set_status(format!("Squash failed: {}", e)),
            }
        } else {
            self.set_status("No selection. Use V or Shift+↑↓ to select.");
        }
    }

    pub fn drop_at_cursor(&mut self) {
        if self.stack.is_empty() {
            return;
        }
        match self.stack.drop_patch(self.cursor) {
            Ok(dropped) => {
                self.record("remove commit");
                self.clamp_cursor();
                self.set_status(format!("Removed: {}", dropped.subject));
            }
            Err(e) => self.set_status(format!("Remove failed: {}", e)),
        }
    }

    /// Insert a new commit at HEAD (top of stack).
    pub fn insert_at_head(&mut self) {
        self.wants_suspend = Some(SuspendReason::InsertAtHead);
    }

    /// Insert a new commit above the cursor position.
    pub fn insert_above_cursor(&mut self) {
        if self.stack.is_empty() {
            self.insert_at_head();
            return;
        }
        let hash = self.stack.patches[self.cursor].hash[..7.min(self.stack.patches[self.cursor].hash.len())].to_string();
        self.wants_suspend = Some(SuspendReason::InsertAboveCursor { hash });
    }

    /// Edit/amend the commit at cursor.
    pub fn edit_commit_at_cursor(&mut self) {
        if self.stack.is_empty() {
            return;
        }
        let hash = self.stack.patches[self.cursor].hash[..7.min(self.stack.patches[self.cursor].hash.len())].to_string();
        self.wants_suspend = Some(SuspendReason::EditCommit { hash });
    }

    pub fn reload_stack(&mut self) -> Result<()> {
        let repo = crate::git::ops::Repo::open()?;
        let commits = repo.list_stack_commits()?;
        self.stack = Stack::new(self.stack.base.clone(), commits);
        self.record("reload");
        self.clamp_cursor();
        Ok(())
    }

    pub fn start_rebase(&mut self) {
        self.mode = Mode::Confirm {
            prompt: format!("Rebase onto {}? (y/n)", self.stack.base),
            action: PendingAction::Rebase,
        };
    }

    pub fn execute_rebase(&mut self) -> Result<bool> {
        let repo = crate::git::ops::Repo::open()?;
        let clean = repo.rebase_onto_base()?;
        if clean {
            self.reload_stack()?;
            self.set_status("Rebase completed successfully.");
            Ok(true)
        } else {
            let conflicts = repo.conflicted_files().unwrap_or_default();
            self.set_status(format!(
                "CONFLICT in {}: {}. Resolve, stage, then c=continue a=abort.",
                conflicts.len(),
                conflicts.join(", ")
            ));
            self.wants_suspend = Some(SuspendReason::RebaseConflict);
            Ok(false)
        }
    }

    pub fn continue_rebase(&mut self) -> Result<bool> {
        let repo = crate::git::ops::Repo::open()?;
        let clean = repo.rebase_continue()?;
        if clean {
            self.reload_stack()?;
            self.set_status("Rebase completed successfully.");
            Ok(true)
        } else {
            let conflicts = repo.conflicted_files().unwrap_or_default();
            self.set_status(format!(
                "CONFLICT in {}: {}. Resolve, stage, then c=continue a=abort.",
                conflicts.len(),
                conflicts.join(", ")
            ));
            self.wants_suspend = Some(SuspendReason::RebaseConflict);
            Ok(false)
        }
    }

    pub fn abort_rebase(&mut self) -> Result<()> {
        let repo = crate::git::ops::Repo::open()?;
        repo.rebase_abort()?;
        self.reload_stack()?;
        self.set_status("Rebase aborted.");
        Ok(())
    }

    pub fn submit_at_cursor(&mut self) {
        let cmd = match &self.submit_cmd {
            Some(c) => c.clone(),
            None => {
                self.set_status("No submit command. Set PGIT_SUBMIT_CMD (e.g. \"arc diff HEAD^\").");
                return;
            }
        };
        if self.stack.is_empty() {
            return;
        }
        let hash = self.stack.patches[self.cursor].hash.clone();
        let subject = self.stack.patches[self.cursor].subject.clone();
        match crate::git::ops::Repo::open().and_then(|r| r.run_submit_cmd(&cmd, &hash, &subject)) {
            Ok(output) => self.set_status(format!("Submitted: {}", output.trim())),
            Err(e) => self.set_status(format!("Submit failed: {}", e)),
        }
    }

    pub fn show_help(&mut self) {
        self.mode = Mode::Help;
    }

    /// Set status with help hint appended.
    pub fn set_status(&mut self, msg: impl Into<String>) {
        let msg = msg.into();
        self.status_msg = format!("{} (?: help)", msg);
    }

    pub fn reset_status(&mut self) {
        self.status_msg = HELP_MSG.to_string();
    }

    pub fn help_text(&self) -> &'static str {
        HELP_FULL
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

    fn key_alt(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::ALT)
    }

    fn key_ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    // --- cursor direction ---

    #[test]
    fn test_initial_cursor_at_top() {
        let app = make_app(5);
        assert_eq!(app.cursor, 4);
    }

    #[test]
    fn test_cursor_up_stops_at_top() {
        let mut app = make_app(3);
        app.cursor = 2;
        app.move_cursor_up();
        assert_eq!(app.cursor, 2);
    }

    #[test]
    fn test_cursor_down_stops_at_bottom() {
        let mut app = make_app(3);
        app.cursor = 0;
        app.move_cursor_down();
        assert_eq!(app.cursor, 0);
    }

    #[test]
    fn test_j_moves_down_k_moves_up() {
        let mut app = make_app(5);
        assert_eq!(app.cursor, 4);
        app.handle_key(key(KeyCode::Char('j'))); // down = decrement
        assert_eq!(app.cursor, 3);
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.cursor, 2);
        app.handle_key(key(KeyCode::Char('k'))); // up = increment
        assert_eq!(app.cursor, 3);
    }

    #[test]
    fn test_g_top_and_capital_g_bottom() {
        let mut app = make_app(5);
        app.cursor = 2;
        app.handle_key(key(KeyCode::Char('g'))); // top = newest
        assert_eq!(app.cursor, 4);
        app.handle_key(key(KeyCode::Char('G'))); // bottom = oldest
        assert_eq!(app.cursor, 0);
    }

    // --- select ---

    #[test]
    fn test_v_enters_select() {
        let mut app = make_app(5);
        app.cursor = 2;
        app.handle_key(key(KeyCode::Char('V')));
        assert_eq!(app.mode, Mode::Select);
        assert_eq!(app.select_anchor, Some(2));
    }

    #[test]
    fn test_shift_down_enters_select() {
        let mut app = make_app(5);
        app.cursor = 2;
        app.handle_key(key_shift(KeyCode::Down));
        assert_eq!(app.mode, Mode::Select);
        assert_eq!(app.select_anchor, Some(2));
        assert_eq!(app.cursor, 1); // down = decrement
    }

    #[test]
    fn test_shift_up_enters_select() {
        let mut app = make_app(5);
        app.cursor = 2;
        app.handle_key(key_shift(KeyCode::Up));
        assert_eq!(app.mode, Mode::Select);
        assert_eq!(app.select_anchor, Some(2));
        assert_eq!(app.cursor, 3); // up = increment
    }

    #[test]
    fn test_select_extend_and_squash() {
        let mut app = make_app(5);
        app.cursor = 3;
        app.handle_key(key(KeyCode::Char('V')));
        app.handle_key(key(KeyCode::Char('j'))); // extend down
        assert_eq!(app.cursor, 2);
        assert_eq!(app.selection_range(), Some((2, 3)));
        app.handle_key(key(KeyCode::Char('s'))); // squash confirm
        assert!(matches!(app.mode, Mode::Confirm { .. }));
        app.handle_key(key(KeyCode::Char('y')));
        assert_eq!(app.stack.len(), 4);
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn test_select_cancel() {
        let mut app = make_app(5);
        app.handle_key(key(KeyCode::Char('V')));
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.select_anchor, None);
    }

    // --- reorder with Alt ---

    #[test]
    fn test_alt_up_moves_patch_up() {
        let mut app = make_app(4);
        app.cursor = 2;
        app.handle_key(key_alt(KeyCode::Up));
        assert_eq!(app.cursor, 3);
        assert_eq!(app.stack.patches[3].subject, "commit 2");
        assert_eq!(app.stack.patches[2].subject, "commit 3");
    }

    #[test]
    fn test_alt_down_moves_patch_down() {
        let mut app = make_app(4);
        app.cursor = 2;
        app.handle_key(key_alt(KeyCode::Down));
        assert_eq!(app.cursor, 1);
        assert_eq!(app.stack.patches[1].subject, "commit 2");
        assert_eq!(app.stack.patches[2].subject, "commit 1");
    }

    #[test]
    fn test_alt_k_moves_patch_up() {
        let mut app = make_app(4);
        app.cursor = 1;
        app.handle_key(key_alt(KeyCode::Char('k')));
        assert_eq!(app.cursor, 2);
        assert_eq!(app.stack.patches[2].subject, "commit 1");
    }

    #[test]
    fn test_alt_j_moves_patch_down() {
        let mut app = make_app(4);
        app.cursor = 2;
        app.handle_key(key_alt(KeyCode::Char('j')));
        assert_eq!(app.cursor, 1);
        assert_eq!(app.stack.patches[1].subject, "commit 2");
    }

    // --- remove (x) with confirm ---

    #[test]
    fn test_remove_confirm() {
        let mut app = make_app(3);
        app.cursor = 1;
        app.handle_key(key(KeyCode::Char('x')));
        assert!(matches!(app.mode, Mode::Confirm { action: PendingAction::Drop, .. }));
        app.handle_key(key(KeyCode::Char('y')));
        assert_eq!(app.stack.len(), 2);
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn test_remove_cancel() {
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
        app.cursor = 1;
        app.handle_key(key(KeyCode::Char('x')));
        app.handle_key(key(KeyCode::Char('y')));
        assert_eq!(app.stack.len(), 3);
        app.handle_key(key(KeyCode::Char('u')));
        assert_eq!(app.stack.len(), 4);
        app.handle_key(key_ctrl(KeyCode::Char('r')));
        assert_eq!(app.stack.len(), 3);
    }

    #[test]
    fn test_undo_nothing() {
        let mut app = make_app(3);
        app.handle_key(key(KeyCode::Char('u')));
        assert!(app.status_msg.contains("Nothing to undo"));
    }

    // --- insert/edit/submit/rebase suspend ---

    #[test]
    fn test_i_inserts_at_head() {
        let mut app = make_app(3);
        app.handle_key(key(KeyCode::Char('i')));
        assert_eq!(app.wants_suspend, Some(SuspendReason::InsertAtHead));
    }

    #[test]
    fn test_o_inserts_above_cursor() {
        let mut app = make_app(3);
        app.cursor = 1;
        app.handle_key(key(KeyCode::Char('o')));
        assert!(matches!(app.wants_suspend, Some(SuspendReason::InsertAboveCursor { .. })));
    }

    #[test]
    fn test_e_edits_commit() {
        let mut app = make_app(3);
        app.cursor = 1;
        app.handle_key(key(KeyCode::Char('e')));
        assert!(matches!(app.wants_suspend, Some(SuspendReason::EditCommit { .. })));
    }

    #[test]
    fn test_rebase_triggers_confirm() {
        let mut app = make_app(3);
        app.handle_key(key(KeyCode::Char('R')));
        assert!(matches!(app.mode, Mode::Confirm { action: PendingAction::Rebase, .. }));
    }

    #[test]
    fn test_submit_no_config() {
        let mut app = make_app(3);
        app.submit_cmd = None;
        app.submit_at_cursor();
        assert!(app.status_msg.contains("No submit command"));
    }

    // --- help ---

    #[test]
    fn test_help_mode() {
        let mut app = make_app(3);
        app.handle_key(key(KeyCode::Char('?')));
        assert_eq!(app.mode, Mode::Help);
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.mode, Mode::Normal);
    }

    // --- status hint ---

    #[test]
    fn test_status_includes_help_hint() {
        let mut app = make_app(3);
        app.set_status("Did something");
        assert!(app.status_msg.contains("?: help"));
    }

    // --- misc ---

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
}
