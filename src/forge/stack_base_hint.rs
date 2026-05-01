//! When `[repo].base` is unset, optional detection of the default branch via each forge's CLI.
//!
//! Precedence is implemented in [`crate::git::ops::Repo::resolve_base`]: explicit config, then
//! [`try_from_forge_cli`], then git ref heuristics. Add new [`crate::forge::ForgeKind`] arms here
//! when a CLI can report the hosting default (e.g. GitLab `glab`, Gitea `tea`).

use crate::forge::ForgeKind;
use crate::git::ops::Repo;

/// Best-effort default stack base from the configured forge's CLI (`None` → fall back to git heuristics).
pub fn try_from_forge_cli(repo: &Repo, forge: ForgeKind) -> Option<String> {
    match forge {
        ForgeKind::GitHub => super::github::try_cli_default_stack_base(repo),
        // Future: ForgeKind::GitLab => super::gitlab::try_cli_default_stack_base(repo),
        // Future: ForgeKind::Gitea => super::gitea::try_cli_default_stack_base(repo),
        ForgeKind::GitLab | ForgeKind::Gitea | ForgeKind::Phabricator | ForgeKind::Custom => None,
    }
}
