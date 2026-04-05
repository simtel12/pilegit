use std::path::PathBuf;
use std::process::Command;

/// Helper: create a temp git repo with an initial commit on `main`
/// and a local "origin" remote so detect_base works.
fn setup_repo(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join(format!("pgit-test-{}-{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let git = |args: &[&str]| {
        let out = Command::new("git")
            .current_dir(&dir)
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {} failed: {}",
            args.join(" "), String::from_utf8_lossy(&out.stderr));
    };

    git(&["init", "-b", "main"]);
    git(&["config", "user.name", "Test User"]);
    git(&["config", "user.email", "test@example.com"]);

    // Initial commit on main
    std::fs::write(dir.join("README.md"), "# test\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-m", "initial commit"]);

    // Set up origin pointing to self so detect_base finds origin/main
    git(&["remote", "add", "origin", dir.to_str().unwrap()]);
    git(&["fetch", "origin"]);

    dir
}

/// Helper: add a commit with a file change.
fn add_commit(dir: &PathBuf, filename: &str, content: &str, message: &str) {
    std::fs::write(dir.join(filename), content).unwrap();
    Command::new("git").current_dir(dir)
        .args(["add", "."]).output().unwrap();
    Command::new("git").current_dir(dir)
        .args(["commit", "-m", message]).output().unwrap();
}

fn open_repo(dir: &PathBuf) -> pilegit::git::ops::Repo {
    pilegit::git::ops::Repo { workdir: dir.clone() }
}

fn cleanup(dir: &PathBuf) {
    let _ = std::fs::remove_dir_all(dir);
}

// --- Tests ---

#[test]
fn list_stack_commits_count_and_order() {
    let dir = setup_repo("list");
    add_commit(&dir, "a.txt", "a", "feat: add a");
    add_commit(&dir, "b.txt", "b", "feat: add b");
    add_commit(&dir, "c.txt", "c", "feat: add c");

    let repo = open_repo(&dir);
    let commits = repo.list_stack_commits().unwrap();

    assert_eq!(commits.len(), 3);
    assert_eq!(commits[0].subject, "feat: add a");
    assert_eq!(commits[1].subject, "feat: add b");
    assert_eq!(commits[2].subject, "feat: add c");
    cleanup(&dir);
}

#[test]
fn swap_commits_changes_order() {
    let dir = setup_repo("swap");
    add_commit(&dir, "a.txt", "aaa\n", "feat: first");
    add_commit(&dir, "b.txt", "bbb\n", "feat: second");

    let repo = open_repo(&dir);
    let commits = repo.list_stack_commits().unwrap();
    assert_eq!(commits.len(), 2);

    let hash_a = &commits[0].hash[..7];
    let hash_b = &commits[1].hash[..7];
    let ok = repo.swap_commits(hash_a, hash_b).unwrap();
    assert!(ok, "swap should succeed without conflicts");

    let commits = repo.list_stack_commits().unwrap();
    assert_eq!(commits[0].subject, "feat: second");
    assert_eq!(commits[1].subject, "feat: first");
    cleanup(&dir);
}

#[test]
fn remove_commit_drops_it() {
    let dir = setup_repo("remove");
    add_commit(&dir, "a.txt", "a\n", "feat: keep");
    add_commit(&dir, "b.txt", "b\n", "feat: remove me");
    add_commit(&dir, "c.txt", "c\n", "feat: also keep");

    let repo = open_repo(&dir);
    let commits = repo.list_stack_commits().unwrap();
    let hash_b = &commits[1].hash[..7];

    let ok = repo.remove_commit(hash_b).unwrap();
    assert!(ok, "remove should succeed without conflicts");

    let commits = repo.list_stack_commits().unwrap();
    assert_eq!(commits.len(), 2);
    assert_eq!(commits[0].subject, "feat: keep");
    assert_eq!(commits[1].subject, "feat: also keep");
    cleanup(&dir);
}

#[test]
fn squash_commits_merges_them() {
    let dir = setup_repo("squash");
    add_commit(&dir, "a.txt", "a\n", "feat: part one");
    add_commit(&dir, "b.txt", "b\n", "feat: part two");
    add_commit(&dir, "c.txt", "c\n", "feat: unrelated");

    let repo = open_repo(&dir);
    let commits = repo.list_stack_commits().unwrap();

    let hash_a = commits[0].hash[..7].to_string();
    let hash_b = commits[1].hash[..7].to_string();

    let ok = repo.squash_commits_with_message(
        &[hash_a, hash_b], "feat: combined",
    ).unwrap();
    assert!(ok, "squash should succeed without conflicts");

    let commits = repo.list_stack_commits().unwrap();
    assert_eq!(commits.len(), 2);
    assert_eq!(commits[0].subject, "feat: combined");
    assert_eq!(commits[1].subject, "feat: unrelated");
    cleanup(&dir);
}

#[test]
fn branch_name_sanitized() {
    let dir = setup_repo("brname");
    let repo = open_repo(&dir);

    let name = repo.make_pgit_branch_name("feat: add login page!");
    assert!(name.starts_with("pgit/"));
    assert!(name.contains("test-user"));
    assert!(!name.contains(' '));
    assert!(!name.contains('!'));
    assert!(!name.contains(':'));
    assert_eq!(name, name.to_lowercase());
    cleanup(&dir);
}

#[test]
fn branch_name_truncated() {
    let dir = setup_repo("brtrunc");
    let repo = open_repo(&dir);

    let long = "a".repeat(100);
    let name = repo.make_pgit_branch_name(&long);
    let slug = name.rsplit('/').next().unwrap();
    assert!(slug.len() <= 50);
    cleanup(&dir);
}

#[test]
fn branch_name_stable() {
    let dir = setup_repo("brstable");
    let repo = open_repo(&dir);

    let a = repo.make_pgit_branch_name("feat: something");
    let b = repo.make_pgit_branch_name("feat: something");
    assert_eq!(a, b);
    cleanup(&dir);
}

#[test]
fn rebase_onto_base_succeeds() {
    let dir = setup_repo("rebase");
    add_commit(&dir, "a.txt", "a\n", "feat: work");

    let repo = open_repo(&dir);
    let ok = repo.rebase_onto_base(&|_| {}).unwrap();
    assert!(ok, "rebase should complete cleanly");
    cleanup(&dir);
}

#[test]
fn diff_returns_content() {
    let dir = setup_repo("diff");
    add_commit(&dir, "hello.txt", "hello world\n", "add hello");

    let repo = open_repo(&dir);
    let commits = repo.list_stack_commits().unwrap();
    let diff = repo.diff_full(&commits[0].hash).unwrap();

    assert!(diff.contains("hello world"));
    assert!(diff.contains("+hello world"));
    cleanup(&dir);
}
