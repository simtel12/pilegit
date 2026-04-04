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

        // Handle suspend reasons
        match app.wants_suspend.take() {
            Some(SuspendReason::InsertCommit) => {
                handle_insert_suspend(&mut app)?;
            }
            Some(SuspendReason::RebaseConflict) => {
                handle_rebase_suspend(&mut app)?;
            }
            None => break,
        }
    }

    Ok(())
}

/// Suspend the TUI for the user to create a new commit.
fn handle_insert_suspend(app: &mut App) -> Result<()> {
    println!("\n\x1b[1;36m┌─ pilegit: insert commit ─────────────────────┐\x1b[0m");
    println!("\x1b[1;36m│\x1b[0m                                               \x1b[1;36m│\x1b[0m");
    println!("\x1b[1;36m│\x1b[0m  Make your changes and commit with:            \x1b[1;36m│\x1b[0m");
    println!("\x1b[1;36m│\x1b[0m    \x1b[1;33mgit add <files> && git commit -m \"...\"\x1b[0m      \x1b[1;36m│\x1b[0m");
    println!("\x1b[1;36m│\x1b[0m                                               \x1b[1;36m│\x1b[0m");
    println!("\x1b[1;36m│\x1b[0m  Press \x1b[1;32mEnter\x1b[0m when done to return to pilegit.   \x1b[1;36m│\x1b[0m");
    println!("\x1b[1;36m└───────────────────────────────────────────────┘\x1b[0m\n");
    io::stdout().flush()?;

    // Wait for Enter
    let mut buf = String::new();
    io::stdin().read_line(&mut buf)?;

    // Reload the stack from git
    match app.reload_stack() {
        Ok(()) => app.status_msg = "Stack refreshed with new commits.".into(),
        Err(e) => app.status_msg = format!("Failed to reload: {}", e),
    }

    Ok(())
}

/// Suspend the TUI for the user to resolve rebase conflicts.
fn handle_rebase_suspend(app: &mut App) -> Result<()> {
    loop {
        // Show conflict information
        let repo = Repo::open()?;
        let conflicts = repo.conflicted_files().unwrap_or_default();

        println!("\n\x1b[1;31m┌─ pilegit: rebase conflict ────────────────────┐\x1b[0m");
        println!("\x1b[1;31m│\x1b[0m                                               \x1b[1;31m│\x1b[0m");
        if conflicts.is_empty() {
            println!("\x1b[1;31m│\x1b[0m  No remaining conflicts detected.             \x1b[1;31m│\x1b[0m");
        } else {
            println!("\x1b[1;31m│\x1b[0m  Conflicting files:                           \x1b[1;31m│\x1b[0m");
            for f in &conflicts {
                println!("\x1b[1;31m│\x1b[0m    \x1b[1;33m{}\x1b[0m", f);
            }
        }
        println!("\x1b[1;31m│\x1b[0m                                               \x1b[1;31m│\x1b[0m");
        println!("\x1b[1;31m│\x1b[0m  Resolve conflicts, then stage with:           \x1b[1;31m│\x1b[0m");
        println!("\x1b[1;31m│\x1b[0m    \x1b[1;33mgit add <resolved files>\x1b[0m                   \x1b[1;31m│\x1b[0m");
        println!("\x1b[1;31m│\x1b[0m                                               \x1b[1;31m│\x1b[0m");
        println!("\x1b[1;31m│\x1b[0m  Then press:                                  \x1b[1;31m│\x1b[0m");
        println!("\x1b[1;31m│\x1b[0m    \x1b[1;32mc\x1b[0m = continue rebase                       \x1b[1;31m│\x1b[0m");
        println!("\x1b[1;31m│\x1b[0m    \x1b[1;31ma\x1b[0m = abort rebase                          \x1b[1;31m│\x1b[0m");
        println!("\x1b[1;31m└───────────────────────────────────────────────┘\x1b[0m\n");
        io::stdout().flush()?;

        print!("  > ");
        io::stdout().flush()?;

        let mut buf = String::new();
        io::stdin().read_line(&mut buf)?;
        let choice = buf.trim();

        match choice {
            "c" => {
                match app.continue_rebase() {
                    Ok(true) => {
                        // Rebase complete
                        app.status_msg = "Rebase completed successfully.".into();
                        return Ok(());
                    }
                    Ok(false) => {
                        // More conflicts — loop again
                        continue;
                    }
                    Err(e) => {
                        app.status_msg = format!("Rebase continue failed: {}", e);
                        return Ok(());
                    }
                }
            }
            "a" => {
                match app.abort_rebase() {
                    Ok(()) => app.status_msg = "Rebase aborted.".into(),
                    Err(e) => app.status_msg = format!("Rebase abort failed: {}", e),
                }
                return Ok(());
            }
            _ => {
                println!("  Invalid choice. Press 'c' to continue or 'a' to abort.");
            }
        }
    }
}
