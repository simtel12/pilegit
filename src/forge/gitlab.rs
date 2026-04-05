use std::collections::HashMap;
use std::process::Command;

use color_eyre::{eyre::eyre, Result};

use super::Forge;
use crate::core::stack::{PatchEntry, PatchStatus};
use crate::git::ops::Repo;

pub struct GitLab;

impl Forge for GitLab {
    fn name(&self) -> &str { "GitLab" }

    fn submit(
        &self, repo: &Repo, hash: &str, subject: &str,
        base: &str, body: &str,
    ) -> Result<String> {
        let branch = repo.get_current_branch()?;
        let branch_name = repo.make_pgit_branch_name(subject);

        repo.git_pub(&["branch", "-f", &branch_name, hash])?;
        repo.git_pub(&["push", "-f", "origin", &branch_name])?;

        let create = Command::new("glab")
            .current_dir(&repo.workdir)
            .args(["mr", "create",
                "--source-branch", &branch_name,
                "--target-branch", base,
                "--title", subject,
                "--description", body,
                "--yes"])
            .output()?;

        let _ = repo.git_pub(&["checkout", "--quiet", &branch]);

        if create.status.success() {
            let url = String::from_utf8_lossy(&create.stdout).trim().to_string();
            return Ok(format!("MR created: {}", url));
        }

        let stderr = String::from_utf8_lossy(&create.stderr);
        if stderr.contains("already exists") {
            self.edit_base(repo, &branch_name, base);
            return Ok(format!("MR updated: {} → {}", branch_name, base));
        }

        Err(eyre!("glab mr create failed: {}", stderr))
    }

    fn update(
        &self, repo: &Repo, hash: &str, subject: &str, base: &str,
    ) -> Result<String> {
        let _ = repo.fetch_origin();
        let branch_name = repo.make_pgit_branch_name(subject);

        repo.git_pub(&["branch", "-f", &branch_name, hash])?;
        repo.git_pub(&["push", "-f", "origin", &branch_name])?;

        self.edit_base(repo, &branch_name, base);
        Ok(format!("MR updated: {} → {}", branch_name, base))
    }

    fn list_open(&self, repo: &Repo) -> (HashMap<String, u32>, bool) {
        let (full, available) = self.list_open_full(repo);
        let map = full.into_iter()
            .map(|(k, (num, _url))| (k, num))
            .collect();
        (map, available)
    }

    fn edit_base(&self, repo: &Repo, branch: &str, base: &str) -> bool {
        // glab mr update requires the MR IID, not branch name
        let iid = self.get_mr_iid(repo, branch);
        let target = match &iid {
            Some(n) => n.as_str(),
            None => branch, // fallback: glab tries current branch's MR
        };

        Command::new("glab")
            .current_dir(&repo.workdir)
            .args(["mr", "update", target, "--target-branch", base])
            .stderr(std::process::Stdio::null())
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn mark_submitted(&self, repo: &Repo, patches: &mut [PatchEntry]) {
        let (mr_map, available) = self.list_open_full(repo);
        for patch in patches.iter_mut() {
            let branch = repo.make_pgit_branch_name(&patch.subject);
            if available {
                if let Some((mr_num, mr_url)) = mr_map.get(&branch) {
                    patch.status = PatchStatus::Submitted;
                    patch.pr_branch = Some(branch);
                    patch.pr_number = Some(*mr_num);
                    patch.pr_url = Some(mr_url.clone());
                }
            } else if repo.git_pub(&["rev-parse", "--verify", &branch]).is_ok() {
                patch.status = PatchStatus::Submitted;
                patch.pr_branch = Some(branch);
            }
        }
    }

    fn sync(
        &self, repo: &Repo, patches: &[PatchEntry],
        on_progress: &dyn Fn(&str),
    ) -> Result<Vec<String>> {
        on_progress("Fetching latest from origin...");
        let _ = repo.fetch_origin();

        let base = repo.detect_base()?;
        let base_branch = base.strip_prefix("origin/").unwrap_or(&base).to_string();

        on_progress("Checking open MRs on GitLab...");
        let (open_mrs, _) = self.list_open(repo);
        let mut updates = Vec::new();

        for (i, patch) in patches.iter().enumerate() {
            let branch = repo.make_pgit_branch_name(&patch.subject);
            if !open_mrs.contains_key(&branch) { continue; }

            on_progress(&format!("Syncing: {} ...", &patch.subject));

            let correct_base = repo.walk_stack_for_base(patches, i, &open_mrs, &base_branch);

            let _ = repo.git_pub(&["branch", "-f", &branch, &patch.hash]);
            let _ = repo.git_pub(&["push", "-f", "origin", &branch]);

            let edited = self.edit_base(repo, &branch, &correct_base);
            let status = if edited { "✓" } else { "⚠" };
            updates.push(format!("{} {} → {}", status, branch, correct_base));
        }

        Ok(updates)
    }
}

impl GitLab {
    /// Fetch open MRs with full data (IID + URL).
    fn list_open_full(&self, repo: &Repo) -> (HashMap<String, (u32, String)>, bool) {
        let mut map = HashMap::new();
        let output = Command::new("glab")
            .current_dir(&repo.workdir)
            .args(["mr", "list", "--mine",
                "--json", "iid,source_branch,web_url"])
            .output();
        match output {
            Ok(out) if out.status.success() => {
                let json = String::from_utf8_lossy(&out.stdout);
                if let Ok(mrs) = serde_json::from_str::<Vec<serde_json::Value>>(&json) {
                    for mr in mrs {
                        // glab may use snake_case or camelCase depending on version
                        let num = mr["iid"].as_u64();
                        let head = mr["source_branch"].as_str()
                            .or_else(|| mr["sourceBranch"].as_str());
                        let url = mr["web_url"].as_str()
                            .or_else(|| mr["webUrl"].as_str())
                            .unwrap_or("").to_string();

                        if let (Some(num), Some(head)) = (num, head) {
                            if head.starts_with("pgit/") {
                                map.insert(head.to_string(), (num as u32, url));
                            }
                        }
                    }
                }
                (map, true)
            }
            _ => (map, false),
        }
    }

    /// Look up the MR IID for a source branch.
    fn get_mr_iid(&self, repo: &Repo, branch: &str) -> Option<String> {
        let output = Command::new("glab")
            .current_dir(&repo.workdir)
            .args(["mr", "view", branch, "--json", "iid"])
            .stderr(std::process::Stdio::null())
            .output().ok()?;
        if output.status.success() {
            let json = String::from_utf8_lossy(&output.stdout);
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&json) {
                return v["iid"].as_u64().map(|n| n.to_string());
            }
        }
        None
    }
}
