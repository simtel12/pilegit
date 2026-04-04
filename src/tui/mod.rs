pub mod app;
pub mod input;
pub mod ui;

use std::io::{self, Write};

use color_eyre::Result;
use crossterm::{
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::core::stack::Stack;
use crate::git::ops::Repo;
use app::{App, SuspendReason};

pub type Tui = Terminal<CrosstermBackend<io::Stdout>>;

pub fn run() -> Result<()> {
    let repo = Repo::open()?;
    let base = repo.detect_base()?;
    let commits = repo.list_stack_commits()?;
    let stack = Stack::new(base, commits);
    let mut app = App::new(stack);

    loop {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        let original_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |panic| {
            let _ = disable_raw_mode();
            let _ = execute!(io::stdout(), LeaveAlternateScreen);
            original_hook(panic);
        }));

        app.run(&mut terminal)?;

        disable_raw_mode()?;
        execute!(terminal.backend_mut(), LeaveAlternateScreen)?;

        if app.should_quit {
            break;
        }

        match app.wants_suspend.take() {
            Some(SuspendReason::InsertAtHead) => handle_insert_at_head(&mut app)?,
            Some(SuspendReason::InsertAboveCursor { hash }) => handle_insert_above(&mut app, &hash)?,
            Some(SuspendReason::EditCommit { hash }) => handle_edit_commit(&mut app, &hash)?,
            Some(SuspendReason::EditSquashMessage { patch_index }) => {
                handle_edit_squash_message(&mut app, patch_index)?
            }
            Some(SuspendReason::RebaseConflict) => handle_rebase_conflict(&mut app)?,
            None => break,
        }
    }

    Ok(())
}

// -------------------------------------------------------------------
// Helpers
// -------------------------------------------------------------------

fn print_box(color: &str, title: &str, lines: &[&str]) {
    let bar = "─".repeat(55);
    println!("\n\x1b[1;{color}m┌─ {title} {bar}\x1b[0m");
    for line in lines {
        println!("\x1b[1;{color}m│\x1b[0m  {line}");
    }
    println!("\x1b[1;{color}m└{bar}──\x1b[0m\n");
}

fn wait_for_enter() -> Result<()> {
    io::stdout().flush()?;
    let mut buf = String::new();
    io::stdin().read_line(&mut buf)?;
    Ok(())
}

// -------------------------------------------------------------------
// Suspend handlers
// -------------------------------------------------------------------

fn handle_insert_at_head(app: &mut App) -> Result<()> {
    print_box("36", "pilegit: insert commit at top", &[
        "Make your changes and commit:",
        "",
        "  \x1b[1;33mgit add <files> && git commit -m \"...\"\x1b[0m",
        "",
        "Press \x1b[1;32mEnter\x1b[0m when done to return.",
    ]);
    wait_for_enter()?;
    match app.reload_stack() {
        Ok(()) => app.notify("Stack refreshed with new commit."),
        Err(e) => app.notify(format!("Reload failed: {}", e)),
    }
    Ok(())
}

fn handle_insert_above(app: &mut App, hash: &str) -> Result<()> {
    let repo = Repo::open()?;
    match repo.rebase_break_after(hash) {
        Ok(_) => {
            print_box("36", &format!("pilegit: insert above {}", hash), &[
                "Rebase paused. Make your changes and commit:",
                "",
                "  \x1b[1;33mgit add <files> && git commit -m \"...\"\x1b[0m",
                "",
                "Press \x1b[1;32mEnter\x1b[0m when done. pilegit will continue the rebase.",
            ]);
            wait_for_enter()?;
            match repo.rebase_continue() {
                Ok(true) => {
                    app.reload_stack()?;
                    app.notify("Inserted commit and rebased.");
                }
                Ok(false) => {
                    app.notify("Conflict during rebase after insert.");
                    app.wants_suspend = Some(SuspendReason::RebaseConflict);
                }
                Err(e) => app.notify(format!("Rebase continue failed: {}", e)),
            }
        }
        Err(e) => app.notify(format!("Insert failed: {}", e)),
    }
    Ok(())
}

fn handle_edit_commit(app: &mut App, hash: &str) -> Result<()> {
    let repo = Repo::open()?;
    match repo.rebase_edit_commit(hash) {
        Ok(true) => app.notify("Commit not found in stack range."),
        Ok(false) => {
            print_box("33", &format!("pilegit: editing {}", hash), &[
                "Rebase paused at this commit. Make your changes:",
                "",
                "  \x1b[1;33mgit add <files>\x1b[0m",
                "  \x1b[1;33mgit commit --amend\x1b[0m",
                "",
                "Press \x1b[1;32mEnter\x1b[0m when done. pilegit will rebase the rest.",
            ]);
            wait_for_enter()?;
            match repo.rebase_continue() {
                Ok(true) => {
                    app.reload_stack()?;
                    app.notify("Commit edited and rebased.");
                }
                Ok(false) => {
                    app.notify("Conflict during rebase after edit.");
                    app.wants_suspend = Some(SuspendReason::RebaseConflict);
                }
                Err(e) => app.notify(format!("Rebase continue failed: {}", e)),
            }
        }
        Err(e) => app.notify(format!("Edit failed: {}", e)),
    }
    Ok(())
}

fn handle_edit_squash_message(app: &mut App, patch_index: usize) -> Result<()> {
    if patch_index >= app.stack.len() {
        app.notify("Squash message edit: invalid patch index.");
        return Ok(());
    }

    let patch = &app.stack.patches[patch_index];
    let initial_content = if patch.body.is_empty() {
        format!("{}\n", patch.subject)
    } else {
        format!("{}\n\n{}\n", patch.subject, patch.body)
    };

    // Write to temp file
    let tmp_path = std::env::temp_dir().join(format!("pgit-squash-msg-{}.txt", std::process::id()));
    std::fs::write(&tmp_path, &initial_content)?;

    // Open $EDITOR (or vi as fallback)
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
    println!("\n  Opening {} to edit the squashed commit message...\n", editor);

    let status = std::process::Command::new(&editor)
        .arg(&tmp_path)
        .status();

    match status {
        Ok(s) if s.success() => {
            // Read back
            let edited = std::fs::read_to_string(&tmp_path)?;
            let _ = std::fs::remove_file(&tmp_path);

            let edited = edited.trim().to_string();
            if edited.is_empty() {
                app.notify("Empty message — kept original.");
                return Ok(());
            }

            // First line = subject, rest = body
            let mut lines = edited.lines();
            let subject = lines.next().unwrap_or("").to_string();
            let body: String = lines
                .collect::<Vec<&str>>()
                .join("\n")
                .trim()
                .to_string();

            app.stack.patches[patch_index].subject = subject;
            app.stack.patches[patch_index].body = body;
            app.notify("Commit message updated.");
        }
        Ok(_) => {
            let _ = std::fs::remove_file(&tmp_path);
            app.notify("Editor exited with error — kept original message.");
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp_path);
            app.notify(format!("Could not open {}: {}", editor, e));
        }
    }

    Ok(())
}

fn handle_rebase_conflict(app: &mut App) -> Result<()> {
    loop {
        let repo = Repo::open()?;
        let conflicts = repo.conflicted_files().unwrap_or_default();

        let mut lines: Vec<String> = vec![];
        if conflicts.is_empty() {
            lines.push("No remaining conflicts detected.".into());
        } else {
            lines.push("Conflicting files:".into());
            for f in &conflicts {
                lines.push(format!("  \x1b[1;33m{}\x1b[0m", f));
            }
        }
        lines.extend_from_slice(&[
            String::new(),
            "Resolve conflicts, then stage: \x1b[1;33mgit add <files>\x1b[0m".into(),
            String::new(),
            "\x1b[1;32mc\x1b[0m = continue rebase    \x1b[1;31ma\x1b[0m = abort rebase".into(),
        ]);

        let line_refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        print_box("31", "pilegit: rebase conflict", &line_refs);

        print!("  > ");
        io::stdout().flush()?;
        let mut buf = String::new();
        io::stdin().read_line(&mut buf)?;

        match buf.trim() {
            "c" => match app.continue_rebase() {
                Ok(true) => return Ok(()),
                Ok(false) => continue,
                Err(e) => { app.notify(format!("Continue failed: {}", e)); return Ok(()); }
            },
            "a" => {
                let _ = app.abort_rebase();
                return Ok(());
            }
            _ => println!("  Press 'c' to continue or 'a' to abort."),
        }
    }
}
