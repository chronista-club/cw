use crate::config;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::fs;

/// Create a new worker environment
pub fn new_worker(name: &str, branch: &str) -> Result<(), String> {
    let worker_dir = setup_worker(name, branch)?;
    println!("{}", worker_dir.display());
    Ok(())
}

/// Fork current dirty state into a new worker environment
pub fn fork_worker(name: &str, branch: &str) -> Result<(), String> {
    let repo_root = config::find_repo_root().map_err(|e| e.to_string())?;

    // Capture dirty state as a diff BEFORE creating the worker
    let diff = capture_dirty_diff(&repo_root)?;

    let worker_dir = setup_worker(name, branch)?;

    // Apply the captured diff to the worker
    if let Some(patch) = diff {
        eprintln!("Applying dirty state...");
        apply_patch(&worker_dir, &patch)?;
    } else {
        eprintln!("No uncommitted changes to fork.");
    }

    println!("{}", worker_dir.display());
    Ok(())
}

/// Common worker setup: clone, symlink, branch, post-setup.
/// Returns the worker directory path.
fn setup_worker(name: &str, branch: &str) -> Result<PathBuf, String> {
    config::validate_worker_name(name)?;

    let repo_root = config::find_repo_root().map_err(|e| e.to_string())?;
    let remote_url = config::get_remote_url().map_err(|e| e.to_string())?;
    let cfg = config::load_config(&repo_root)?;
    let workers_dir = config::workers_dir()?;

    // Auto-prefix with repo name if not already included
    let repo_name = config::repo_name().unwrap_or_default();
    let actual_name = apply_repo_prefix(name, &repo_name);
    let worker_dir = workers_dir.join(&actual_name);

    // Clean up existing
    if worker_dir.exists() {
        eprintln!("Cleaning up existing worker: {}", worker_dir.display());
        fs::remove_dir_all(&worker_dir).map_err(|e| e.to_string())?;
    }

    // Clone
    fs::create_dir_all(&workers_dir).map_err(|e| e.to_string())?;
    eprintln!("Cloning to {}...", worker_dir.display());
    let repo_root_str = repo_root.to_str()
        .ok_or("repo root path is not valid UTF-8")?;
    let worker_dir_str = worker_dir.to_str()
        .ok_or("worker dir path is not valid UTF-8")?;
    run_git(&["clone", "--depth", "1", repo_root_str, worker_dir_str])?;

    // Set remote to GitHub URL
    run_git_in(&worker_dir, &["remote", "set-url", "origin", &remote_url])?;

    // Symlinks
    for file in &cfg.symlinks {
        let src = repo_root.join(file);
        let dst = worker_dir.join(file);
        if src.exists() {
            if let Some(parent) = dst.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            // Remove existing file from clone (if it exists) before symlinking
            let _ = fs::remove_file(&dst);
            std::os::unix::fs::symlink(&src, &dst).map_err(|e| e.to_string())?;
            eprintln!("  symlink: {file}");
        }
    }

    // Copies
    for file in &cfg.copies {
        let src = repo_root.join(file);
        let dst = worker_dir.join(file);
        if src.exists() {
            if let Some(parent) = dst.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            fs::copy(&src, &dst).map_err(|e| e.to_string())?;
            eprintln!("  copy: {file}");
        }
    }

    // Symlink patterns
    for pattern in &cfg.symlink_patterns {
        let matches = glob::glob(&format!("{}/{pattern}", repo_root.display()))
            .map_err(|e| e.to_string())?;

        for entry in matches.flatten() {
            // Skip .git directory
            if entry.to_str().is_some_and(|s| s.contains("/.git/")) {
                continue;
            }
            if let Ok(rel) = entry.strip_prefix(&repo_root) {
                let dst = worker_dir.join(rel);
                if !dst.exists() {
                    if let Some(parent) = dst.parent() {
                        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                    }
                    std::os::unix::fs::symlink(&entry, &dst).map_err(|e| e.to_string())?;
                    eprintln!("  symlink (pattern): {}", rel.display());
                }
            }
        }
    }

    // Create branch
    run_git_in(&worker_dir, &["checkout", "-b", branch])?;

    // Post-setup
    if let Some(cmd) = &cfg.post_setup {
        eprintln!("Running: {cmd}");
        let status = Command::new("sh")
            .args(["-c", cmd])
            .current_dir(&worker_dir)
            .status()
            .map_err(|e| e.to_string())?;

        if !status.success() {
            return Err(format!("post-setup failed: {cmd}"));
        }
    }

    Ok(worker_dir)
}

/// List all worker environments
pub fn list_workers() -> Result<(), String> {
    let workers_dir = config::workers_dir()?;
    if !workers_dir.exists() {
        return Ok(());
    }

    let entries = fs::read_dir(&workers_dir).map_err(|e| e.to_string())?;

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();

        // Get current branch
        let branch = get_branch(&path).unwrap_or_else(|| "-".to_string());
        println!("{name}\t{branch}\t{}", path.display());
    }

    Ok(())
}

/// Print the path to a worker
pub fn worker_path(name: &str) -> Result<(), String> {
    let workers_dir = config::workers_dir()?;
    let worker_dir = workers_dir.join(name);
    if worker_dir.exists() {
        println!("{}", worker_dir.display());
        return Ok(());
    }
    // Fallback: try with repo name prefix
    if let Some(repo_name) = config::repo_name() {
        let prefixed = workers_dir.join(format!("{repo_name}-{name}"));
        if prefixed.exists() {
            println!("{}", prefixed.display());
            return Ok(());
        }
    }
    Err(format!("worker '{name}' not found. Run `ccws ls` to see available workers."))
}

/// Remove a worker environment
pub fn remove_worker(name: Option<&str>, all: bool, force: bool) -> Result<(), String> {
    let workers_dir = config::workers_dir()?;

    if all {
        if !force {
            return Err("--all requires --force to prevent accidental deletion".into());
        }
        if workers_dir.exists() {
            fs::remove_dir_all(&workers_dir).map_err(|e| e.to_string())?;
            eprintln!("Removed all workers");
        }
        return Ok(());
    }

    let name = name.ok_or("specify a worker name or --all --force")?;
    config::validate_worker_name(name)?;

    let worker_dir = workers_dir.join(name);
    if worker_dir.exists() {
        fs::remove_dir_all(&worker_dir).map_err(|e| e.to_string())?;
        eprintln!("Removed worker: {name}");
        return Ok(());
    }
    // Fallback: try with repo name prefix
    if let Some(repo_name) = config::repo_name() {
        let prefixed_name = format!("{repo_name}-{name}");
        let prefixed_dir = workers_dir.join(&prefixed_name);
        if prefixed_dir.exists() {
            fs::remove_dir_all(&prefixed_dir).map_err(|e| e.to_string())?;
            eprintln!("Removed worker: {prefixed_name}");
            return Ok(());
        }
    }
    Err(format!("worker '{name}' not found. Run `ccws ls` to see available workers."))
}

/// Show status of all worker environments
pub fn status_workers() -> Result<(), String> {
    let workers_dir = config::workers_dir()?;
    if !workers_dir.exists() {
        eprintln!("ワーカーはありません。`ccws new <name> <branch>` で作成できます。");
        return Ok(());
    }

    let entries = fs::read_dir(&workers_dir).map_err(|e| e.to_string())?;
    let mut found = false;

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() || !path.join(".git").exists() {
            continue;
        }
        found = true;

        let name = entry.file_name();
        let name = name.to_string_lossy();
        let branch = get_branch(&path).unwrap_or_else(|| "-".to_string());
        let changes = count_changes(&path);
        let ahead_behind = get_ahead_behind(&path);
        let last_commit = get_last_commit(&path);

        let changes_str = if changes > 0 {
            format!("{changes} files")
        } else {
            "clean".to_string()
        };

        println!("{name}\t{branch}\t{changes_str}\t{ahead_behind}\t{last_commit}");
    }

    if !found {
        eprintln!("ワーカーはありません。`ccws new <name> <branch>` で作成できます。");
    }

    Ok(())
}

/// Remove workers whose branch is merged into main
pub fn cleanup_workers(force: bool) -> Result<(), String> {
    let workers_dir = config::workers_dir()?;
    if !workers_dir.exists() {
        eprintln!("クリーンアップ対象はありません。");
        return Ok(());
    }

    let entries = fs::read_dir(&workers_dir).map_err(|e| e.to_string())?;
    let mut to_remove: Vec<(String, std::path::PathBuf)> = Vec::new();
    let mut kept: Vec<(String, String)> = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() || !path.join(".git").exists() {
            continue;
        }

        let name = entry.file_name().to_string_lossy().to_string();

        // Fetch latest remote state in each worker
        let _ = run_git_in(&path, &["fetch", "--quiet"]);

        if is_branch_merged(&path) {
            to_remove.push((name, path));
        } else {
            let changes = count_changes(&path);
            let reason = if changes > 0 {
                format!("アクティブ ({changes} files changed)")
            } else {
                "未マージ".to_string()
            };
            kept.push((name, reason));
        }
    }

    if to_remove.is_empty() {
        eprintln!("クリーンアップ対象はありません。");
        for (name, reason) in &kept {
            eprintln!("  保持: {name} ({reason})");
        }
        return Ok(());
    }

    for (name, _) in &to_remove {
        eprintln!("  削除可能: {name} (マージ済み)");
    }
    for (name, reason) in &kept {
        eprintln!("  保持: {name} ({reason})");
    }

    if !force {
        eprintln!("\n実際に削除するには `ccws cleanup --force` を実行してください。");
        return Ok(());
    }

    for (name, path) in &to_remove {
        fs::remove_dir_all(path).map_err(|e| e.to_string())?;
        eprintln!("  削除: {name}");
    }

    eprintln!("{} ワーカーを削除しました。", to_remove.len());
    Ok(())
}

// --- helpers ---

/// Apply repo name prefix to worker name, avoiding double-prefixing.
/// e.g. ("issue-42", "nexus") → "nexus-issue-42"
///      ("nexus-issue-42", "nexus") → "nexus-issue-42"
pub(crate) fn apply_repo_prefix(name: &str, repo_name: &str) -> String {
    if !repo_name.is_empty() && !name.starts_with(&format!("{repo_name}-")) {
        format!("{repo_name}-{name}")
    } else {
        name.to_string()
    }
}

/// Capture uncommitted changes (staged + unstaged + untracked) as a combined diff.
/// Returns None if there are no changes.
fn capture_dirty_diff(repo_root: &Path) -> Result<Option<String>, String> {
    // Staged + unstaged tracked changes
    let tracked = Command::new("git")
        .args(["diff", "HEAD"])
        .current_dir(repo_root)
        .output()
        .map_err(|e| e.to_string())?;

    if !tracked.status.success() {
        return Err("git diff HEAD failed".to_string());
    }

    let diff = String::from_utf8_lossy(&tracked.stdout).to_string();

    // Untracked files — generate diff with --no-index /dev/null <file>
    let untracked = Command::new("git")
        .args(["ls-files", "--others", "--exclude-standard"])
        .current_dir(repo_root)
        .output()
        .map_err(|e| e.to_string())?;

    let mut full_diff = diff;

    if untracked.status.success() {
        for file in String::from_utf8_lossy(&untracked.stdout).lines() {
            let file = file.trim();
            if file.is_empty() {
                continue;
            }
            // Use git diff --no-index to generate a proper patch (handles binary, no-newline, etc.)
            let file_diff = Command::new("git")
                .args(["diff", "--no-index", "--", "/dev/null", file])
                .current_dir(repo_root)
                .output()
                .ok();
            if let Some(output) = file_diff {
                // --no-index exits 1 when files differ (expected), only skip on spawn failure
                let patch = String::from_utf8_lossy(&output.stdout);
                if !patch.is_empty() {
                    // Rewrite paths: /dev/null → a/<file>, <file> → b/<file>
                    for line in patch.lines() {
                        if line.starts_with("+++ ") && !line.contains("/dev/null") {
                            full_diff.push_str(&format!("+++ b/{file}\n"));
                        } else if line.starts_with("--- /dev/null") {
                            full_diff.push_str("--- /dev/null\n");
                        } else if line.starts_with("diff --git") {
                            full_diff.push_str(&format!("diff --git a/{file} b/{file}\n"));
                        } else {
                            full_diff.push_str(line);
                            full_diff.push('\n');
                        }
                    }
                }
            }
        }
    }

    if full_diff.trim().is_empty() {
        Ok(None)
    } else {
        Ok(Some(full_diff))
    }
}

/// Apply a unified diff patch to a directory
fn apply_patch(worker_dir: &Path, patch: &str) -> Result<(), String> {
    let mut child = Command::new("git")
        .args(["apply", "--allow-empty", "-"])
        .current_dir(worker_dir)
        .stdin(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;

    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        stdin.write_all(patch.as_bytes()).map_err(|e| e.to_string())?;
    }

    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git apply failed: {stderr}"));
    }

    Ok(())
}

fn run_git(args: &[&str]) -> Result<(), String> {
    let output = Command::new("git").args(args).output().map_err(|e| e.to_string())?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git {} failed: {stderr}", args.join(" ")));
    }
    Ok(())
}

fn run_git_in(dir: &std::path::Path, args: &[&str]) -> Result<(), String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git {} failed: {stderr}", args.join(" ")));
    }
    Ok(())
}

fn count_changes(dir: &std::path::Path) -> usize {
    let output = Command::new("git")
        .args(["status", "--short"])
        .current_dir(dir)
        .output()
        .ok();
    match output {
        Some(o) if o.status.success() => {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter(|l| !l.is_empty())
                .count()
        }
        _ => 0,
    }
}

fn get_ahead_behind(dir: &std::path::Path) -> String {
    let output = Command::new("git")
        .args(["rev-list", "--left-right", "--count", "HEAD...@{upstream}"])
        .current_dir(dir)
        .output()
        .ok();
    match output {
        Some(o) if o.status.success() => {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            let parts: Vec<&str> = s.split('\t').collect();
            if parts.len() == 2 {
                let ahead: i32 = parts[0].parse().unwrap_or(0);
                let behind: i32 = parts[1].parse().unwrap_or(0);
                match (ahead, behind) {
                    (0, 0) => "up-to-date".to_string(),
                    (a, 0) => format!("↑{a}"),
                    (0, b) => format!("↓{b}"),
                    (a, b) => format!("↑{a}↓{b}"),
                }
            } else {
                "-".to_string()
            }
        }
        _ => "local".to_string(),
    }
}

fn get_last_commit(dir: &std::path::Path) -> String {
    let output = Command::new("git")
        .args(["log", "--oneline", "-1"])
        .current_dir(dir)
        .output()
        .ok();
    match output {
        Some(o) if o.status.success() => {
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        }
        _ => "-".to_string(),
    }
}

/// Check if HEAD in the worker dir is merged into origin/main (or origin/master).
/// A worker is "merged" only if:
///   1. HEAD is an ancestor of origin/<main> (merge-base --is-ancestor), AND
///   2. The worker has diverged (has at least 1 local commit beyond origin/<main>)
/// This prevents false positives on freshly-created workers (HEAD == origin/main).
fn is_branch_merged(worker_dir: &std::path::Path) -> bool {
    for branch in &["main", "master"] {
        let remote_ref = format!("origin/{branch}");

        // Check if HEAD is ancestor of remote main
        let ancestor = Command::new("git")
            .args(["merge-base", "--is-ancestor", "HEAD", &remote_ref])
            .current_dir(worker_dir)
            .output()
            .ok();
        if !matches!(ancestor, Some(ref o) if o.status.success()) {
            continue;
        }

        // Guard: skip if worker has no local commits (HEAD == origin/main)
        let diverged = Command::new("git")
            .args(["rev-list", &format!("{remote_ref}..HEAD"), "--count"])
            .current_dir(worker_dir)
            .output()
            .ok();
        let local_commits: usize = diverged
            .as_ref()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok())
            .unwrap_or(0);

        if local_commits == 0 {
            // No divergence — freshly created worker, not "merged"
            continue;
        }

        return true;
    }
    false
}

fn get_branch(dir: &std::path::Path) -> Option<String> {
    let output = Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(dir)
        .output()
        .ok()?;

    if output.status.success() {
        let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if branch.is_empty() { None } else { Some(branch) }
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- apply_repo_prefix ---

    #[test]
    fn prefix_added_when_missing() {
        assert_eq!(apply_repo_prefix("issue-42", "nexus"), "nexus-issue-42");
    }

    #[test]
    fn prefix_not_doubled() {
        assert_eq!(
            apply_repo_prefix("nexus-issue-42", "nexus"),
            "nexus-issue-42"
        );
    }

    #[test]
    fn prefix_empty_repo_name() {
        assert_eq!(apply_repo_prefix("issue-42", ""), "issue-42");
    }

    #[test]
    fn prefix_exact_repo_name_without_dash() {
        // "nexus" != "nexus-" prefix, so it gets prefixed
        assert_eq!(apply_repo_prefix("nexus", "nexus"), "nexus-nexus");
    }

    #[test]
    fn prefix_partial_match_not_confused() {
        // "nexus-pro" starts with "nexus-" so no double prefix
        assert_eq!(apply_repo_prefix("nexus-pro", "nexus"), "nexus-pro");
        // "nexuspro" does NOT start with "nexus-" so it gets prefixed
        assert_eq!(apply_repo_prefix("nexuspro", "nexus"), "nexus-nexuspro");
    }
}
