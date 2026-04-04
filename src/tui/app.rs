use color_eyre::Result;
use crossterm::event::{self, Event, KeyEvent};
use std::time::Duration;

use super::input;
use super::ui;
use super::Tui;
use crate::core::history::History;
use crate::core::stack::Stack;

pub const SHORTCUTS_NORMAL: &str =
    "↑/k ↓/j:move  V/Shift+↑↓:select  Alt+↑↓:reorder  e:edit  i:insert  o:insert above  x:remove  d:diff  R:rebase  S:submit  u:undo  ?:help  q:quit";
pub const SHORTCUTS_SELECT: &str =
    "Shift+↑↓ or j/k:extend  s:squash  Esc:cancel";
pub const SHORTCUTS_DIFF: &str =
    "↑↓/jk:scroll  Ctrl+d/u:page  q:back";

/// Why the TUI is being suspended.
#[derive(Debug, Clone, PartialEq)]
pub enum SuspendReason {
    InsertAtHead,
    InsertAboveCursor { hash: String },
    EditCommit { hash: String },
    EditSquashMessage { patch_index: usize },
    RebaseConflict,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    Normal,
    Select,
    DiffView,
    HistoryView,
    Help,
    Confirm { prompt: String, action: PendingAction },
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
    pub cursor: usize,
    pub select_anchor: Option<usize>,
    pub expanded: Option<usize>,
    pub scroll_offset: usize,
    pub diff_content: Vec<String>,
    pub diff_scroll: usize,
    /// Temporary notification shown above the shortcuts bar
    pub notification: Option<String>,
    pub should_quit: bool,
    pub wants_suspend: Option<SuspendReason>,
    pub submit_cmd: Option<String>,
}

impl App {
    pub fn new(stack: Stack) -> Self {
        let cursor = if stack.is_empty() { 0 } else { stack.len() - 1 };
        let mut history = History::new(500);
        history.push("initial", &stack);
        let submit_cmd = std::env::var("PGIT_SUBMIT_CMD").ok();
        Self {
            stack, history,
            mode: Mode::Normal,
            cursor,
            select_anchor: None,
            expanded: None,
            scroll_offset: 0,
            diff_content: Vec::new(),
            diff_scroll: 0,
            notification: None,
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

    /// The shortcut text for the current mode.
    pub fn shortcuts(&self) -> &str {
        match &self.mode {
            Mode::Select => SHORTCUTS_SELECT,
            Mode::DiffView => SHORTCUTS_DIFF,
            Mode::HistoryView => "q/Esc:back",
            Mode::Help => "q/Esc:close",
            Mode::Confirm { .. } => "y:confirm  n/Esc:cancel",
            _ => SHORTCUTS_NORMAL,
        }
    }

    pub fn notify(&mut self, msg: impl Into<String>) {
        self.notification = Some(msg.into());
    }

    pub fn clear_notification(&mut self) {
        self.notification = None;
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
            self.notify(format!("Undone ({}/{})", self.history.position(), self.history.total()));
        } else {
            self.notify("Nothing to undo.");
        }
    }

    pub fn redo(&mut self) {
        if let Some(next) = self.history.redo() {
            self.stack = next.clone();
            self.clamp_cursor();
            self.notify(format!("Redone ({}/{})", self.history.position(), self.history.total()));
        } else {
            self.notify("Nothing to redo.");
        }
    }

    pub fn clamp_cursor(&mut self) {
        if self.stack.is_empty() {
            self.cursor = 0;
        } else if self.cursor >= self.stack.len() {
            self.cursor = self.stack.len() - 1;
        }
    }

    // Visual cursor: up = newer = higher index, down = older = lower index

    pub fn move_cursor_up(&mut self) {
        self.clear_notification();
        if !self.stack.is_empty() && self.cursor < self.stack.len() - 1 {
            self.cursor += 1;
        }
    }

    pub fn move_cursor_down(&mut self) {
        self.clear_notification();
        if self.cursor > 0 {
            self.cursor -= 1;
        }
    }

    pub fn move_patch_up(&mut self) {
        if !self.stack.is_empty() && self.cursor < self.stack.len() - 1 {
            let _ = self.stack.reorder(self.cursor, self.cursor + 1);
            self.cursor += 1;
            self.record("move patch up");
            self.notify("Patch moved up.");
        }
    }

    pub fn move_patch_down(&mut self) {
        if self.cursor > 0 && !self.stack.is_empty() {
            let _ = self.stack.reorder(self.cursor, self.cursor - 1);
            self.cursor -= 1;
            self.record("move patch down");
            self.notify("Patch moved down.");
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
                    // Suspend to let the user edit the squashed commit message
                    self.wants_suspend = Some(SuspendReason::EditSquashMessage {
                        patch_index: lo,
                    });
                    self.notify(format!("Squashed {} commits. Opening editor for commit message...", count));
                }
                Err(e) => self.notify(format!("Squash failed: {}", e)),
            }
        } else {
            self.notify("No selection. Use V or Shift+↑↓ to select.");
        }
    }

    pub fn drop_at_cursor(&mut self) {
        if self.stack.is_empty() { return; }
        match self.stack.drop_patch(self.cursor) {
            Ok(dropped) => {
                self.record("remove commit");
                self.clamp_cursor();
                self.notify(format!("Removed: {}", dropped.subject));
            }
            Err(e) => self.notify(format!("Remove failed: {}", e)),
        }
    }

    pub fn insert_at_head(&mut self) {
        self.wants_suspend = Some(SuspendReason::InsertAtHead);
    }

    pub fn insert_above_cursor(&mut self) {
        if self.stack.is_empty() {
            self.insert_at_head();
            return;
        }
        let h = &self.stack.patches[self.cursor].hash;
        let hash = h[..7.min(h.len())].to_string();
        self.wants_suspend = Some(SuspendReason::InsertAboveCursor { hash });
    }

    pub fn edit_commit_at_cursor(&mut self) {
        if self.stack.is_empty() { return; }
        let h = &self.stack.patches[self.cursor].hash;
        let hash = h[..7.min(h.len())].to_string();
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
            self.notify("Rebase completed successfully.");
            Ok(true)
        } else {
            let conflicts = repo.conflicted_files().unwrap_or_default();
            self.notify(format!("CONFLICT: {}", conflicts.join(", ")));
            self.wants_suspend = Some(SuspendReason::RebaseConflict);
            Ok(false)
        }
    }

    pub fn continue_rebase(&mut self) -> Result<bool> {
        let repo = crate::git::ops::Repo::open()?;
        let clean = repo.rebase_continue()?;
        if clean {
            self.reload_stack()?;
            self.notify("Rebase completed successfully.");
            Ok(true)
        } else {
            let conflicts = repo.conflicted_files().unwrap_or_default();
            self.notify(format!("CONFLICT: {}", conflicts.join(", ")));
            self.wants_suspend = Some(SuspendReason::RebaseConflict);
            Ok(false)
        }
    }

    pub fn abort_rebase(&mut self) -> Result<()> {
        let repo = crate::git::ops::Repo::open()?;
        repo.rebase_abort()?;
        self.reload_stack()?;
        self.notify("Rebase aborted.");
        Ok(())
    }

    pub fn submit_at_cursor(&mut self) {
        let cmd = match &self.submit_cmd {
            Some(c) => c.clone(),
            None => {
                self.notify("No submit command. Set PGIT_SUBMIT_CMD (e.g. \"arc diff HEAD^\").");
                return;
            }
        };
        if self.stack.is_empty() { return; }
        let hash = self.stack.patches[self.cursor].hash.clone();
        let subject = self.stack.patches[self.cursor].subject.clone();
        match crate::git::ops::Repo::open().and_then(|r| r.run_submit_cmd(&cmd, &hash, &subject)) {
            Ok(output) => self.notify(format!("Submitted: {}", output.trim())),
            Err(e) => self.notify(format!("Submit failed: {}", e)),
        }
    }

    pub fn show_help(&mut self) {
        self.mode = Mode::Help;
    }

    pub fn help_text(&self) -> &'static str {
        "\
Navigation:  ↑/k up  ↓/j down  g top  G bottom  Enter expand
Select:      Shift+↑↓ start select  V select  j/k extend  s squash  Esc cancel
Reorder:     Alt+↑↓ or Alt+k/j move patch
Edit:        e edit/amend commit  i insert at top  o insert above cursor
Stack:       R rebase  S submit  x remove  d diff
History:     u undo  Ctrl+r redo  h history view
             q quit  ? this help"
    }
}

