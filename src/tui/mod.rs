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

/// Launch the interactive TUI with suspend/resume support.
pub fn run() -> Result<()> {
    let repo = Repo::open()?;
    let base = repo.detect_base()?;
    let commits = repo.list_stack_commits()?;
    let stack = Stack::new(base, commits);
    let mut app = App::new(stack);

    loop {
        // Initialize terminal
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        // Install panic hook
        let original_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |panic| {
            let _ = disable_raw_mode();
            let _ = execute!(io::stdout(), LeaveAlternateScreen);
            original_hook(panic);
        }));

        // Run the TUI until quit or suspend
        app.run(&mut terminal)?;

        // Restore terminal
        disable_raw_mode()?;
        execute!(terminal.backend_mut(), LeaveAlternateScreen)?;

        if app.should_quit {
            break;
        }

        match app.wants_suspend.take() {
            Some(SuspendReason::InsertAtHead) => handle_insert_at_head(&mut app)?,
            Some(SuspendReason::InsertAboveCursor { hash }) => {
                handle_insert_above(&mut app, &hash)?
            }
            Some(SuspendReason::EditCommit { hash }) => handle_edit_commit(&mut app, &hash)?,
            Some(SuspendReason::RebaseConflict) => handle_rebase_conflict(&mut app)?,
            None => break,
        }
    }

    Ok(())
}

// -------------------------------------------------------------------
// Suspend handlers — each prints instructions, waits, then resumes
// -------------------------------------------------------------------

fn print_box(color: &str, title: &str, lines: &[&str]) {
    let width = 55;
    let bar = "─".repeat(width - 2);
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

fn handle_insert_at_head(app: &mut App) -> Result<()> {
    print_box(
        "36",
        "pilegit: insert commit at top",
        &[
            "Make your changes and commit:",
            "",
            "  \x1b[1;33mgit add <files>\x1b[0m",
            "  \x1b[1;33mgit commit -m \"your message\"\x1b[0m",
            "",
            "Press \x1b[1;32mEnter\x1b[0m when done to return to pilegit.",
        ],
    );
    wait_for_enter()?;
    match app.reload_stack() {
        Ok(()) => app.set_status("Stack refreshed with new commit."),
        Err(e) => app.set_status(format!("Reload failed: {}", e)),
    }
    Ok(())
}

fn handle_insert_above(app: &mut App, hash: &str) -> Result<()> {
    // Start an interactive rebase with a break after the target commit
    let repo = Repo::open()?;
    match repo.rebase_break_after(hash) {
        Ok(_) => {
            print_box(
                "36",
                &format!("pilegit: insert commit above {}", hash),
                &[
                    "Rebase paused. Make your changes and commit:",
                    "",
                    "  \x1b[1;33mgit add <files>\x1b[0m",
                    "  \x1b[1;33mgit commit -m \"your message\"\x1b[0m",
                    "",
                    "Press \x1b[1;32mEnter\x1b[0m when done. pilegit will continue the rebase.",
                ],
            );
            wait_for_enter()?;

            // Continue the rebase to replay remaining commits
            match repo.rebase_continue() {
                Ok(true) => {
                    app.reload_stack()?;
                    app.set_status("Inserted commit and rebased successfully.");
                }
                Ok(false) => {
                    // Conflicts during rebase — enter conflict handler
                    app.set_status("Conflict during rebase after insert.");
                    app.wants_suspend = Some(SuspendReason::RebaseConflict);
                }
                Err(e) => app.set_status(format!("Rebase continue failed: {}", e)),
            }
        }
        Err(e) => app.set_status(format!("Insert failed: {}", e)),
    }
    Ok(())
}

fn handle_edit_commit(app: &mut App, hash: &str) -> Result<()> {
    // Start an interactive rebase with the target commit marked as "edit"
    let repo = Repo::open()?;
    match repo.rebase_edit_commit(hash) {
        Ok(true) => {
            // Rebase completed without stopping — commit wasn't in range
            app.set_status("Commit not found in stack range.");
        }
        Ok(false) => {
            // Rebase paused at the commit for editing
            print_box(
                "33",
                &format!("pilegit: editing commit {}", hash),
                &[
                    "Rebase paused at this commit. Make your changes:",
                    "",
                    "  \x1b[1;33mgit add <files>\x1b[0m",
                    "  \x1b[1;33mgit commit --amend\x1b[0m",
                    "",
                    "Press \x1b[1;32mEnter\x1b[0m when done. pilegit will rebase the rest.",
                ],
            );
            wait_for_enter()?;

            // Continue the rebase to replay remaining commits
            match repo.rebase_continue() {
                Ok(true) => {
                    app.reload_stack()?;
                    app.set_status("Commit edited and rebased successfully.");
                }
                Ok(false) => {
                    // Conflicts — enter conflict handler
                    app.set_status("Conflict during rebase after edit.");
                    app.wants_suspend = Some(SuspendReason::RebaseConflict);
                }
                Err(e) => app.set_status(format!("Rebase continue failed: {}", e)),
            }
        }
        Err(e) => app.set_status(format!("Edit failed: {}", e)),
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
        lines.push(String::new());
        lines.push("Resolve conflicts, then stage:".into());
        lines.push("  \x1b[1;33mgit add <resolved files>\x1b[0m".into());
        lines.push(String::new());
        lines.push("Then press:".into());
        lines.push("  \x1b[1;32mc\x1b[0m = continue rebase".into());
        lines.push("  \x1b[1;31ma\x1b[0m = abort rebase".into());

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
                Err(e) => {
                    app.set_status(format!("Rebase continue failed: {}", e));
                    return Ok(());
                }
            },
            "a" => {
                match app.abort_rebase() {
                    Ok(()) => {}
                    Err(e) => app.set_status(format!("Abort failed: {}", e)),
                }
                return Ok(());
            }
            _ => println!("  Press 'c' to continue or 'a' to abort."),
        }
    }
}
