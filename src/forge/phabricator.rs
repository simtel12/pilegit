use std::collections::HashMap;
use std::process::Command;

use color_eyre::{eyre::eyre, Result};

use super::Forge;
use crate::core::stack::{PatchEntry, PatchStatus};
use crate::git::ops::Repo;

/// Phabricator integration via `arc` CLI.
///
/// Key insight: `arc diff` amends the commit to add a `Differential Revision:`
/// trailer. To keep this trailer in the branch history, submit/update use
/// interactive rebase (`edit` marker) so the amendment happens in-place.
pub struct Phabricator;

impl Forge for Phabricator {
    fn name(&self) -> &str { "Phabricator" }
    fn needs_description_editor(&self) -> bool { false }

    fn submit(
        &self, repo: &Repo, hash: &str, _subject: &str,
        _base: &str, _body: &str,
    ) -> Result<String> {
        let short = &hash[..7.min(hash.len())];

        // Pause rebase at the target commit
        match repo.rebase_edit_commit(short) {
            Ok(false) => {} // paused — good
            Ok(true) => return Err(eyre!("Commit {} not found in stack", short)),
            Err(e) => return Err(eyre!("Failed to start rebase: {}", e)),
        }

        // Run arc diff interactively — arc creates a revision and amends the
        // commit to add a `Differential Revision:` trailer.
        let status = Command::new("arc")
            .current_dir(&repo.workdir)
            .args(["diff", "HEAD^"])
            .status();

        // Parse revision ID from the (possibly amended) commit
        let msg = repo.git_pub(&["log", "-1", "--format=%B"])
            .unwrap_or_default();
        let revision_id = parse_revision_id(&msg);

        // Continue rebase to replay the rest of the stack
        let rebase_ok = match repo.rebase_continue() {
            Ok(true) => true,
            Ok(false) => false, // conflicts
            Err(_) => false,
        };

        match status {
            Ok(s) if s.success() => {
                let id_str = revision_id
                    .map(|id| format!("D{}", id))
                    .unwrap_or_else(|| "unknown".to_string());
                if rebase_ok {
                    Ok(format!("Revision created: {}", id_str))
                } else {
                    Ok(format!("Revision created: {} (rebase has conflicts — resolve and run `git rebase --continue`)", id_str))
                }
            }
            Ok(_) => {
                // arc failed — abort the rebase to restore state
                let _ = repo.rebase_abort();
                Err(eyre!("arc diff failed"))
            }
            Err(e) => {
                let _ = repo.rebase_abort();
                Err(eyre!("arc not found: {}", e))
            }
        }
    }

    fn update(
        &self, repo: &Repo, hash: &str, _subject: &str, _base: &str,
    ) -> Result<String> {
        let short = &hash[..7.min(hash.len())];

        // Get revision ID from the commit message before rebasing
        let msg = repo.git_pub(&["log", "-1", "--format=%B", hash])
            .unwrap_or_default();
        let revision_id = parse_revision_id(&msg);

        // Pause rebase at the target commit
        match repo.rebase_edit_commit(short) {
            Ok(false) => {}
            Ok(true) => return Err(eyre!("Commit {} not found in stack", short)),
            Err(e) => return Err(eyre!("Failed to start rebase: {}", e)),
        }

        // Run arc diff to update the existing revision
        let status = match &revision_id {
            Some(id) => {
                Command::new("arc")
                    .current_dir(&repo.workdir)
                    .args(["diff", "HEAD^", "--update", &format!("D{}", id)])
                    .status()
            }
            None => {
                // No revision ID — arc will try to detect from commit message
                Command::new("arc")
                    .current_dir(&repo.workdir)
                    .args(["diff", "HEAD^"])
                    .status()
            }
        };

        // Continue rebase
        let rebase_ok = match repo.rebase_continue() {
            Ok(true) => true,
            Ok(false) => false,
            Err(_) => false,
        };

        match status {
            Ok(s) if s.success() => {
                let id_str = revision_id
                    .map(|id| format!("D{}", id))
                    .unwrap_or_else(|| "unknown".to_string());
                if rebase_ok {
                    Ok(format!("Revision updated: {}", id_str))
                } else {
                    Ok(format!("Revision updated: {} (rebase has conflicts — resolve and run `git rebase --continue`)", id_str))
                }
            }
            Ok(_) => {
                let _ = repo.rebase_abort();
                Err(eyre!("arc diff failed"))
            }
            Err(e) => {
                let _ = repo.rebase_abort();
                Err(eyre!("arc not found: {}", e))
            }
        }
    }

    fn list_open(&self, _repo: &Repo) -> (HashMap<String, u32>, bool) {
        (HashMap::new(), false)
    }

    fn edit_base(&self, _repo: &Repo, _branch: &str, _base: &str) -> bool {
        true
    }

    fn mark_submitted(&self, repo: &Repo, patches: &mut [PatchEntry]) {
        // Scan commit messages for "Differential Revision:" trailers.
        // These trailers are added by arc diff during submit and preserved
        // through rebases, so they persist in the branch history.
        for patch in patches.iter_mut() {
            let full = repo.git_pub(&["log", "-1", "--format=%B", &patch.hash])
                .unwrap_or_default();
            if let Some(id) = parse_revision_id(&full) {
                patch.status = PatchStatus::Submitted;
                patch.pr_number = Some(id);
            }
        }
    }

    fn sync(
        &self, repo: &Repo, patches: &[PatchEntry],
        on_progress: &dyn Fn(&str),
    ) -> Result<Vec<String>> {
        // For sync, we use detached HEAD since we just need to upload the
        // latest diff — the Differential Revision trailer is already in the
        // commit from the initial submit. arc detects it automatically.
        let branch = repo.get_current_branch()?;
        let mut updates = Vec::new();

        for patch in patches {
            if patch.status != PatchStatus::Submitted { continue; }
            let id = match patch.pr_number {
                Some(id) => id,
                None => continue,
            };

            on_progress(&format!("Updating D{}: {} ...", id, &patch.subject));

            if repo.git_pub(&["checkout", "--quiet", &patch.hash]).is_err() {
                updates.push(format!("⚠ D{} checkout failed, skipping", id));
                continue;
            }

            // Non-interactive: skip editor, provide message, close stdin
            let status = Command::new("arc")
                .current_dir(&repo.workdir)
                .args(["diff", "HEAD^", "--update", &format!("D{}", id),
                    "--verbatim", "--message", &patch.subject])
                .stdin(std::process::Stdio::null())
                .status();

            match status {
                Ok(s) if s.success() => {
                    updates.push(format!("✓ D{} updated", id));
                }
                _ => {
                    updates.push(format!("⚠ D{} update failed", id));
                }
            }
        }

        let _ = repo.git_pub(&["checkout", "--quiet", &branch]);
        Ok(updates)
    }
}

/// Parse a Phabricator revision ID from a commit message.
/// Looks for "Differential Revision: .../DXXXX" or just "D" followed by digits.
fn parse_revision_id(message: &str) -> Option<u32> {
    for line in message.lines() {
        let line = line.trim();
        if line.starts_with("Differential Revision:") {
            if let Some(d_pos) = line.rfind('D') {
                let num_str: String = line[d_pos + 1..]
                    .chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect();
                if let Ok(id) = num_str.parse::<u32>() {
                    return Some(id);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_revision_url() {
        let msg = "Some commit\n\nDifferential Revision: https://phab.example.com/D1234";
        assert_eq!(parse_revision_id(msg), Some(1234));
    }

    #[test]
    fn parse_revision_bare() {
        assert_eq!(parse_revision_id("Differential Revision: D5678"), Some(5678));
    }

    #[test]
    fn parse_revision_multiline() {
        let msg = "fix bug\n\nDifferential Revision: https://phab.co/D42\nSome other line";
        assert_eq!(parse_revision_id(msg), Some(42));
    }

    #[test]
    fn parse_revision_not_present() {
        assert_eq!(parse_revision_id("just a commit message"), None);
        assert_eq!(parse_revision_id(""), None);
    }

    #[test]
    fn parse_revision_wrong_prefix() {
        assert_eq!(parse_revision_id("Reviewed-by: D9999"), None);
    }
}
