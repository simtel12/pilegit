use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::app::{App, Mode, PendingAction};

pub fn handle_normal(app: &mut App, key: KeyEvent) {
    if key.modifiers.contains(KeyModifiers::SHIFT) {
        match key.code {
            KeyCode::Up => {
                app.clear_notification();
                app.select_anchor = Some(app.cursor);
                app.mode = Mode::Select;
                app.move_cursor_up();
            }
            KeyCode::Down => {
                app.clear_notification();
                app.select_anchor = Some(app.cursor);
                app.mode = Mode::Select;
                app.move_cursor_down();
            }
            _ => {}
        }
        return;
    }

    if key.modifiers.contains(KeyModifiers::ALT) {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => app.move_patch_up(),
            KeyCode::Down | KeyCode::Char('j') => app.move_patch_down(),
            _ => {}
        }
        return;
    }

    if key.modifiers.contains(KeyModifiers::CONTROL) {
        if let KeyCode::Char('r') = key.code {
            app.redo();
        }
        return;
    }

    match key.code {
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Char('k') | KeyCode::Up => app.move_cursor_up(),
        KeyCode::Char('j') | KeyCode::Down => app.move_cursor_down(),
        KeyCode::Char('g') => {
            app.clear_notification();
            if !app.stack.is_empty() { app.cursor = app.stack.len() - 1; }
        }
        KeyCode::Char('G') => {
            app.clear_notification();
            app.cursor = 0;
        }
        KeyCode::Char('V') => {
            app.clear_notification();
            app.select_anchor = Some(app.cursor);
            app.mode = Mode::Select;
        }
        KeyCode::Enter | KeyCode::Char(' ') => {
            app.clear_notification();
            if app.expanded == Some(app.cursor) {
                app.expanded = None;
            } else {
                app.expanded = Some(app.cursor);
            }
        }
        KeyCode::Char('d') => {
            app.clear_notification();
            if !app.stack.is_empty() {
                let hash = app.stack.patches[app.cursor].hash.clone();
                match crate::git::ops::Repo::open().and_then(|r| r.diff_full(&hash)) {
                    Ok(diff) => {
                        app.diff_content = diff.lines().map(|l| l.to_string()).collect();
                        app.diff_scroll = 0;
                        app.mode = Mode::DiffView;
                    }
                    Err(e) => app.notify(format!("diff error: {}", e)),
                }
            }
        }
        KeyCode::Char('e') => app.edit_commit_at_cursor(),
        KeyCode::Char('i') => app.insert_at_head(),
        KeyCode::Char('o') => app.insert_above_cursor(),
        KeyCode::Char('x') => {
            if !app.stack.is_empty() {
                let subject = app.stack.patches[app.cursor].subject.clone();
                app.mode = Mode::Confirm {
                    prompt: format!("Remove '{}'? (y/n)", subject),
                    action: PendingAction::Drop,
                };
            }
        }
        KeyCode::Char('R') => app.start_rebase(),
        KeyCode::Char('S') => app.submit_at_cursor(),
        KeyCode::Char('u') => app.undo(),
        KeyCode::Char('h') => {
            app.clear_notification();
            app.mode = Mode::HistoryView;
        }
        KeyCode::Char('?') => app.show_help(),
        _ => {}
    }
}

pub fn handle_select(app: &mut App, key: KeyEvent) {
    if key.modifiers.contains(KeyModifiers::SHIFT) {
        match key.code {
            KeyCode::Up => app.move_cursor_up(),
            KeyCode::Down => app.move_cursor_down(),
            _ => {}
        }
        return;
    }

    match key.code {
        KeyCode::Char('k') | KeyCode::Up => app.move_cursor_up(),
        KeyCode::Char('j') | KeyCode::Down => app.move_cursor_down(),
        KeyCode::Char('s') => {
            if let Some((lo, hi)) = app.selection_range() {
                let count = hi - lo + 1;
                app.mode = Mode::Confirm {
                    prompt: format!("Squash {} commits? (y/n)", count),
                    action: PendingAction::Squash,
                };
            }
        }
        KeyCode::Esc | KeyCode::Char('q') => {
            app.select_anchor = None;
            app.mode = Mode::Normal;
        }
        _ => {}
    }
}

pub fn handle_diff_view(app: &mut App, key: KeyEvent) {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('d') => {
                app.diff_scroll = app.diff_scroll.saturating_add(20)
                    .min(app.diff_content.len().saturating_sub(1));
            }
            KeyCode::Char('u') => {
                app.diff_scroll = app.diff_scroll.saturating_sub(20);
            }
            _ => {}
        }
        return;
    }

    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => {
            app.mode = Mode::Normal;
            app.diff_content.clear();
        }
        KeyCode::Char('j') | KeyCode::Down => {
            if app.diff_scroll < app.diff_content.len().saturating_sub(1) {
                app.diff_scroll += 1;
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.diff_scroll = app.diff_scroll.saturating_sub(1);
        }
        _ => {}
    }
}

pub fn handle_history_view(app: &mut App, key: KeyEvent) {
    if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) {
        app.mode = Mode::Normal;
    }
}

pub fn handle_help(app: &mut App, key: KeyEvent) {
    if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc | KeyCode::Char('?')) {
        app.mode = Mode::Normal;
    }
}

pub fn handle_confirm(app: &mut App, key: KeyEvent) {
    let (action, confirmed) = match key.code {
        KeyCode::Char('y') | KeyCode::Enter => {
            if let Mode::Confirm { ref action, .. } = app.mode {
                (Some(action.clone()), true)
            } else {
                (None, false)
            }
        }
        KeyCode::Char('n') | KeyCode::Esc => (None, false),
        _ => return,
    };

    app.mode = Mode::Normal;

    if confirmed {
        if let Some(action) = action {
            match action {
                PendingAction::Squash => app.squash_selected(),
                PendingAction::Drop => app.drop_at_cursor(),
                PendingAction::Rebase => {
                    if let Err(e) = app.execute_rebase() {
                        app.notify(format!("Rebase error: {}", e));
                    }
                }
            }
        }
    } else {
        app.notify("Cancelled.");
    }
}
