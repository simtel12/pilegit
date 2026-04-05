use std::io::{self, Write};
use std::path::{Path, PathBuf};

use color_eyre::Result;
use serde::{Deserialize, Serialize};

const CONFIG_FILE: &str = ".pilegit.toml";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub forge: ForgeConfig,
    #[serde(default)]
    pub repo: RepoConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForgeConfig {
    /// Platform type: github, gitlab, gitea, phabricator, custom
    #[serde(rename = "type")]
    pub forge_type: String,
    /// Custom submit command (only used when forge_type = "custom")
    pub submit_cmd: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RepoConfig {
    /// Base branch override (e.g. "origin/main"). Auto-detected if not set.
    pub base: Option<String>,
}

impl Config {
    /// Load config from `.pilegit.toml` in the repo root.
    pub fn load(repo_root: &Path) -> Option<Config> {
        let path = repo_root.join(CONFIG_FILE);
        let content = std::fs::read_to_string(path).ok()?;
        toml::from_str(&content).ok()
    }

    /// Save config to `.pilegit.toml`.
    pub fn save(&self, repo_root: &Path) -> Result<()> {
        let path = repo_root.join(CONFIG_FILE);
        let content = toml::to_string_pretty(self)?;
        std::fs::write(&path, content)?;
        Ok(())
    }

    /// Config file path for a repo.
    pub fn _path(repo_root: &Path) -> PathBuf {
        repo_root.join(CONFIG_FILE)
    }
}

/// Interactive setup wizard. Returns a new Config.
pub fn run_setup(repo_root: &Path) -> Result<Config> {
    println!();
    println!("  \x1b[1;36m▸ pilegit setup\x1b[0m");
    println!();
    println!("  Which code review platform do you use?");
    println!();
    println!("    \x1b[1;33m1\x1b[0m  GitHub      (uses \x1b[33mgh\x1b[0m CLI)");
    println!("    \x1b[1;33m2\x1b[0m  GitLab      (uses \x1b[33mglab\x1b[0m CLI)");
    println!("    \x1b[1;33m3\x1b[0m  Gitea       (uses \x1b[33mtea\x1b[0m CLI)");
    println!("    \x1b[1;33m4\x1b[0m  Phabricator (uses \x1b[33marc\x1b[0m CLI)");
    println!("    \x1b[1;33m5\x1b[0m  Custom command");
    println!();

    let forge_type = loop {
        print!("  Select [1-5]: ");
        io::stdout().flush()?;
        let mut buf = String::new();
        io::stdin().read_line(&mut buf)?;
        match buf.trim() {
            "1" => break "github".to_string(),
            "2" => break "gitlab".to_string(),
            "3" => break "gitea".to_string(),
            "4" => break "phabricator".to_string(),
            "5" => break "custom".to_string(),
            _ => println!("  Please enter 1-5."),
        }
    };

    let submit_cmd = if forge_type == "custom" {
        println!();
        println!("  Enter your submit command template.");
        println!("  Placeholders: {{hash}}, {{subject}}, {{message}}, {{message_file}}");
        println!();
        print!("  Command: ");
        io::stdout().flush()?;
        let mut buf = String::new();
        io::stdin().read_line(&mut buf)?;
        let cmd = buf.trim().to_string();
        if cmd.is_empty() { None } else { Some(cmd) }
    } else {
        None
    };

    // Auto-detect or ask for base branch
    let detected_base = crate::git::ops::Repo::open()
        .and_then(|r| r.detect_base())
        .ok();

    println!();
    if let Some(ref base) = detected_base {
        println!("  Base branch detected: \x1b[1;32m{}\x1b[0m", base);
        print!("  Use this? (Enter to accept, or type a different branch): ");
    } else {
        print!("  Base branch (e.g. origin/main): ");
    }
    io::stdout().flush()?;
    let mut buf = String::new();
    io::stdin().read_line(&mut buf)?;
    let base_input = buf.trim().to_string();
    let base = if base_input.is_empty() {
        detected_base
    } else {
        Some(base_input)
    };

    let config = Config {
        forge: ForgeConfig { forge_type, submit_cmd },
        repo: RepoConfig { base },
    };

    config.save(repo_root)?;
    println!();
    println!("  \x1b[32m✓ Config saved to {}\x1b[0m", CONFIG_FILE);
    println!();

    Ok(config)
}

/// Check that required CLI tools are installed and print warnings.
/// Returns Ok(()) always — warnings are non-fatal.
pub fn check_dependencies(config: &Config) {
    let mut ok = true;

    // git is always required
    match get_tool_version("git", &["--version"]) {
        Some(v) => {
            if let Some(major_minor) = parse_version(&v) {
                if major_minor < (2, 26) {
                    eprintln!("  \x1b[33m⚠ git {} found — pgit requires git 2.26+\x1b[0m", v);
                    ok = false;
                }
            }
        }
        None => {
            eprintln!("  \x1b[31m✗ git not found. pgit requires git.\x1b[0m");
            ok = false;
        }
    }

    // Platform-specific CLI
    let (tool, min_ver, install_hint) = match config.forge.forge_type.as_str() {
        "github" => ("gh", (2, 0), "https://cli.github.com/"),
        "gitlab" => ("glab", (1, 20), "https://gitlab.com/gitlab-org/cli"),
        "gitea" => ("tea", (0, 9), "https://gitea.com/gitea/tea"),
        "phabricator" => ("arc", (0, 0), "arcanist"),
        _ => return, // custom — no CLI dependency
    };

    match get_tool_version(tool, &["--version"]) {
        Some(v) => {
            if min_ver != (0, 0) {
                if let Some(major_minor) = parse_version(&v) {
                    if major_minor < min_ver {
                        eprintln!(
                            "  \x1b[33m⚠ {} {} found — pgit recommends {}.{}+\x1b[0m",
                            tool, v, min_ver.0, min_ver.1
                        );
                        ok = false;
                    }
                }
            }
        }
        None => {
            eprintln!(
                "  \x1b[31m✗ `{}` not found. Install it: {}\x1b[0m",
                tool, install_hint
            );
            ok = false;
        }
    }

    if ok {
        return; // all good, no output
    }
    eprintln!();
}

/// Run `tool --version` and return the raw output string.
fn get_tool_version(tool: &str, args: &[&str]) -> Option<String> {
    std::process::Command::new(tool)
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// Parse a major.minor version from a version string like "git version 2.43.0".
fn parse_version(version_str: &str) -> Option<(u32, u32)> {
    let digits_start = version_str.find(|c: char| c.is_ascii_digit())?;
    let version_part = &version_str[digits_start..];
    let mut parts = version_part.split('.');
    let major = parts.next()?.parse::<u32>().ok()?;
    let minor = parts.next()?.parse::<u32>().ok()?;
    Some((major, minor))
}
