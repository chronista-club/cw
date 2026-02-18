use crate::config;
use std::process::Command;
use std::fs;

/// Create a new worker environment
pub fn new_worker(name: &str, branch: &str) -> Result<(), String> {
    let repo_root = config::find_repo_root().map_err(|e| e.to_string())?;
    let remote_url = config::get_remote_url().map_err(|e| e.to_string())?;
    let cfg = config::load_config(&repo_root)?;
    let workers_dir = config::workers_dir();
    let worker_dir = workers_dir.join(name);

    // Clean up existing
    if worker_dir.exists() {
        eprintln!("Cleaning up existing worker: {}", worker_dir.display());
        fs::remove_dir_all(&worker_dir).map_err(|e| e.to_string())?;
    }

    // Clone
    fs::create_dir_all(&workers_dir).map_err(|e| e.to_string())?;
    eprintln!("Cloning to {}...", worker_dir.display());
    run_git(&["clone", "--depth", "1", repo_root.to_str().unwrap(), worker_dir.to_str().unwrap()])?;

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
        let name_pattern = pattern.rsplit('/').next().unwrap_or(pattern);
        let matches = glob::glob(&format!("{}/**/{name_pattern}", repo_root.display()))
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

    // Output the path (stdout = composable)
    println!("{}", worker_dir.display());
    Ok(())
}

/// List all worker environments
pub fn list_workers() -> Result<(), String> {
    let workers_dir = config::workers_dir();
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
    let worker_dir = config::workers_dir().join(name);
    if !worker_dir.exists() {
        return Err(format!("worker '{name}' not found"));
    }
    println!("{}", worker_dir.display());
    Ok(())
}

/// Remove a worker environment
pub fn remove_worker(name: Option<&str>, all: bool) -> Result<(), String> {
    let workers_dir = config::workers_dir();

    if all {
        if workers_dir.exists() {
            fs::remove_dir_all(&workers_dir).map_err(|e| e.to_string())?;
            eprintln!("Removed all workers");
        }
        return Ok(());
    }

    let name = name.ok_or("specify a worker name or --all")?;
    let worker_dir = workers_dir.join(name);
    if !worker_dir.exists() {
        return Err(format!("worker '{name}' not found"));
    }
    fs::remove_dir_all(&worker_dir).map_err(|e| e.to_string())?;
    eprintln!("Removed worker: {name}");
    Ok(())
}

// --- helpers ---

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
