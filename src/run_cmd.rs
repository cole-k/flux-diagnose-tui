use crate::types::{CompilerMessage, ErrorAndFixes, GitInformation, RemoteInfo};
use anyhow::{bail, Context, Result};
use git2::{
    ErrorCode, FetchOptions, FetchPrune, Oid, ReferenceType, RemoteCallbacks, Repository, Status,
    StatusOptions,
}; // Added FetchOptions, FetchPrune, RemoteCallbacks
use log::{error, info, warn};
use once_cell::sync::Lazy;
use regex::Regex;
use std::collections::{HashMap, VecDeque};
use std::fs::File;
// Added error level
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

// Removed thread and Duration as we won't sleep after git2 fetch

/// Finds the URL of the first remote whose branch contains the given commit.
/// Iterates through local references under `refs/remotes/*`.
fn find_remote_containing_commit(repo: &Repository, commit_oid: Oid) -> Result<Option<RemoteInfo>> {
    info!(
        "Searching local remote refs for commit {}",
        &commit_oid.to_string()[..7]
    );
    for reference_result in repo.references_glob("refs/remotes/*")? {
        let reference = match reference_result {
            Ok(r) => r,
            Err(e) => {
                warn!("Failed to read a reference during glob: {}", e);
                continue; // Skip this problematic reference
            }
        };

        // Ensure it's a direct reference to a commit
        if !matches!(reference.kind(), Some(ReferenceType::Direct)) {
            continue;
        }

        if let Some(remote_branch_tip_oid) = reference.target() {
            // Check if our target commit is an ancestor of this remote branch tip
            match repo.graph_descendant_of(remote_branch_tip_oid, commit_oid) {
                Ok(true) => {
                    if let Some(ref_name) = reference.name() {
                        let parts: Vec<&str> = ref_name.splitn(4, '/').collect();
                        if parts.len() >= 3 && parts[0] == "refs" && parts[1] == "remotes" {
                            let remote_name = parts[2];
                            info!(
                                "Commit {} found on remote '{}' via local ref '{}'",
                                &commit_oid.to_string()[..7],
                                remote_name,
                                ref_name
                            );
                            match repo.find_remote(remote_name) {
                                Ok(remote) => {
                                    if let Some(url) = remote.url() {
                                        return Ok(Some(RemoteInfo::new(
                                            remote_name.to_string(),
                                            url.to_string(),
                                        )));
                                    } else {
                                        warn!(
                                            "Remote '{}' (from ref '{}') has no URL configured.",
                                            remote_name, ref_name
                                        );
                                    }
                                }
                                Err(e) => warn!(
                                    "Found ref '{}' but failed to find remote config for '{}': {}",
                                    ref_name, remote_name, e
                                ),
                            }
                        } else {
                            warn!("Could not parse remote name from ref: {}", ref_name);
                        }
                    }
                }
                Ok(false) => { /* Commit is not an ancestor, continue */ }
                Err(e) => {
                    if e.code() != ErrorCode::NotFound {
                        // NotFound can happen if OIDs become invalid, less critical
                        warn!(
                            "Error checking commit ancestry for ref {:?} against commit {}: {}",
                            reference.name(),
                            &commit_oid.to_string()[..7],
                            e
                        );
                    }
                }
            }
        }
    }
    info!(
        "Commit {} not found on any existing local remote refs.",
        &commit_oid.to_string()[..7]
    );
    Ok(None) // Not found in any remote branch ref
}

/// Fetches updates from all configured remotes using git2, pruning stale branches.
fn fetch_all_remotes_prune(repo: &Repository) -> Result<()> {
    info!("Attempting to fetch all remotes with prune using git2");
    let remotes = repo.remotes()?;
    if remotes.is_empty() {
        info!("No remotes configured, skipping fetch.");
        return Ok(());
    }

    let mut fetch_errors = Vec::new();

    // --- Configure Fetch Options ---
    let mut callbacks = RemoteCallbacks::new();
    // Basic callbacks to handle credentials (delegates to git credential helper/ssh-agent etc.)
    // More complex scenarios might require implementing custom callbacks.
    callbacks.credentials(|_url, username_from_url, _allowed_types| {
        git2::Cred::default() // Try default system credential helpers
            .or_else(|e| {
                // Fallback to SSH key from agent or standard paths if username is 'git' or not specified
                if username_from_url.is_none_or(|u| u == "git") {
                     git2::Cred::ssh_key_from_agent(username_from_url.unwrap_or("git"))
                } else {
                    Err(e) // If not default and not SSH agent, propagate original error
                }
            })
             .map_err(|e| {
                warn!("Credential lookup failed: {}. If needed, configure git credential helper or ssh-agent.", e);
                e // Return error to signal fetch failure for this remote
            })
    });

    let mut fetch_options = FetchOptions::new();
    fetch_options.remote_callbacks(callbacks);
    fetch_options.prune(FetchPrune::On); // Enable pruning
                                         // fetch_options.download_tags(git2::AutotagOption::All); // Optionally fetch all tags

    for remote_name_res in remotes.iter() {
        match remote_name_res {
            Some(remote_name) => {
                info!("Fetching remote: {}", remote_name);
                match repo.find_remote(remote_name) {
                    Ok(mut remote) => {
                        // `fetch` needs `&mut self`
                        let refspecs: &[&str] = &[]; // Use default refspecs
                        match remote.fetch(refspecs, Some(&mut fetch_options), None) {
                            Ok(_) => {
                                info!("Successfully fetched remote '{}'", remote_name);
                            }
                            Err(e) => {
                                let msg =
                                    format!("Failed to fetch remote '{}': {}", remote_name, e);
                                warn!("{}", msg); // Log as warning
                                fetch_errors.push(msg); // Collect error message
                            }
                        }
                    }
                    Err(e) => {
                        let msg = format!(
                            "Failed to find remote configuration for '{}' listed in remotes: {}",
                            remote_name, e
                        );
                        warn!("{}", msg);
                        fetch_errors.push(msg);
                    }
                }
            }
            None => {
                warn!("Encountered an invalid remote name in the repository's list.");
            }
        }
    }

    if fetch_errors.is_empty() {
        info!("Fetch operation completed for all remotes.");
        Ok(())
    } else {
        // Even if some fetches failed, the operation as a whole "completed",
        // but we signal that *something* went wrong. Depending on requirements,
        // you might want to return Ok(()) here anyway and rely on logs.
        // Returning an error indicates the fetch wasn't fully successful.
        bail!(
            "Fetch completed with errors for some remotes:\n - {}",
            fetch_errors.join("\n - ")
        );
    }
}

/// Represents the status results, including flags and file lists.
#[derive(Debug, Default)]
pub struct RepoStatusInfo {
    pub uncommitted_files: Vec<String>,
    pub untracked_files: Vec<String>,
}

/// Checks a Git repository for uncommitted changes and untracked files.
///
/// # Arguments
///
/// * `repo` - A reference to the `git2::Repository` to check.
///
/// # Returns
///
/// * `Ok(RepoStatusInfo)` - Contains boolean flags and lists of file paths
///   relative to the repository root if the status check succeeds.
/// * `Err(git2::Error)` - If there was an error accessing the repository status.
pub fn check_repo_status(repo: &Repository) -> Result<RepoStatusInfo> {
    // Configure status options:
    // - Include untracked files.
    // - Recurse into untracked directories to find nested untracked files.
    // - Exclude submodules (can be changed if needed).
    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .exclude_submodules(true); // Set to false if you want to check submodules status

    // Get the status collection for the repository
    let statuses = repo.statuses(Some(&mut opts))?;

    // Define the status flags that indicate an "uncommitted change"
    // This includes staged changes (INDEX_*) and unstaged changes (WT_*)
    // excluding WT_NEW which we handle separately as "untracked".
    const UNCOMMITTED_MASK: Status = Status::INDEX_NEW
        .union(Status::INDEX_MODIFIED)
        .union(Status::INDEX_DELETED)
        .union(Status::INDEX_TYPECHANGE)
        .union(Status::INDEX_RENAMED)
        .union(Status::WT_MODIFIED)
        .union(Status::WT_DELETED)
        .union(Status::WT_TYPECHANGE)
        .union(Status::WT_RENAMED);

    let mut result = RepoStatusInfo::default();

    // Iterate over the status entries
    for entry in statuses.iter() {
        let status = entry.status();
        let path = match entry.path() {
            Some(p) => p.to_string(),
            None => continue, // Should typically not happen for status entries
        };

        // Check for untracked files
        if status.contains(Status::WT_NEW) {
            result.untracked_files.push(path.clone()); // Clone needed if also added below
        }

        // Check for uncommitted changes (staged or unstaged modifications/deletions/renames etc.)
        // using the pre-defined mask.
        if status.intersects(UNCOMMITTED_MASK) {
            result.uncommitted_files.push(path);
        }
    }

    // Sort the file lists for consistent output (optional)
    result.uncommitted_files.sort();
    result.untracked_files.sort();

    Ok(result)
}

/// Prints a warning message to stdout if the repository status indicates
/// uncommitted changes or untracked files.
///
/// # Arguments
///
/// * `status_info` - A reference to the `RepoStatusInfo` containing the status details.
pub fn print_repo_status_warning(status_info: &RepoStatusInfo) {
    let has_uncommitted_files = !status_info.uncommitted_files.is_empty();
    let has_untracked_files = !status_info.untracked_files.is_empty();
    // Only print the warning if there's something to warn about
    if !has_uncommitted_files && !has_untracked_files {
        return; // Nothing to warn about, exit early
    }

    // --- Construct the header message dynamically ---
    let mut warning_parts = Vec::new();
    if has_uncommitted_files {
        warning_parts.push("uncommitted");
    }
    if has_untracked_files {
        warning_parts.push("untracked");
    }
    let changes_description = warning_parts.join(" and ");
    println!("WARNING: there are {} changes", changes_description);
    // --- End of header construction ---

    // --- Print Uncommitted Files ---
    if has_uncommitted_files {
        println!("  Uncommitted:");
        for file in &status_info.uncommitted_files {
            println!("     * {}", file);
        }
    }

    // --- Print Untracked Files ---
    if has_untracked_files {
        println!("   Untracked:");
        for file in &status_info.untracked_files {
            println!("     * {}", file);
        }
    }

    // --- Print the final line ---
    println!("Running anyway...");
}

pub fn discover_git_info(directory: &Path) -> Result<(GitInformation, PathBuf)> {
    let canonical_directory = directory.canonicalize().with_context(|| {
        format!(
            "Failed to canonicalize directory path: {}",
            directory.display()
        )
    })?;

    // --- Collect Git Information using git2 ---
    info!(
        "Collecting Git information for directory: {}",
        canonical_directory.display()
    );
    let repo = Repository::discover(&canonical_directory).with_context(|| {
        format!(
            "Failed to discover git repository containing '{}'",
            canonical_directory.display()
        )
    })?;

    let status = check_repo_status(&repo)?;
    print_repo_status_warning(&status);

    // Check if workdir exists early, as we need it for rel_path.
    // Fetch itself might work on bare repos, but our function requires workdir.
    let repo_root = repo.workdir().ok_or_else(|| {
        anyhow::anyhow!(
            "Repository at '{}' is bare, cannot determine relative path or reliably prune.",
            repo.path().display()
        )
    })?;

    let rel_path_from_root =
        pathdiff::diff_paths(&canonical_directory, repo_root).ok_or_else(|| {
            anyhow::anyhow!(
                "Could not determine relative path from repo root ({}) to directory ({})",
                repo_root.display(),
                canonical_directory.display()
            )
        })?;

    let head_ref = repo.head().context("Failed to get HEAD reference")?;
    let commit_oid = head_ref
        .peel_to_commit()
        .context("Failed to peel HEAD reference to a commit (maybe unborn branch?)")?
        .id();

    let branch = match head_ref.shorthand() {
        Some(name) if !name.is_empty() && name != "HEAD" => name.to_string(),
        _ => {
            if head_ref.is_branch() {
                "HEAD (unborn)".to_string()
            } else {
                format!("detached@{}", &commit_oid.to_string()[..7])
            }
        }
    };
    info!(
        "Resolved Git info: Commit {}, Branch '{}', Relative Path '{}'",
        &commit_oid.to_string()[..7],
        branch,
        rel_path_from_root.display()
    );

    // --- Find Remote URL based on Commit ---
    info!(
        "Attempting to find remote URL for commit {}",
        &commit_oid.to_string()[..7]
    );
    let mut remote = find_remote_containing_commit(&repo, commit_oid)?;

    if remote.is_none() {
        info!(
            "Commit {} not found on existing local remote refs. Attempting fetch...",
            &commit_oid.to_string()[..7]
        );
        // Attempt fetch using git2 only if the commit wasn't found initially
        match fetch_all_remotes_prune(&repo) {
            Ok(_) => {
                // Fetch succeeded (or completed with non-fatal errors), try searching again
                info!(
                    "Fetch completed. Re-checking for remote containing commit {}",
                    &commit_oid.to_string()[..7]
                );
                // Re-run the search after fetch
                remote = find_remote_containing_commit(&repo, commit_oid)?;
                if remote.is_some() {
                    info!("Found remote URL after fetch.");
                } else {
                    info!(
                        "Commit {} still not found on any remote refs after fetch.",
                        &commit_oid.to_string()[..7]
                    );
                }
            }
            Err(fetch_err) => {
                // Fetch failed more fundamentally (e.g., couldn't list remotes, or returned bail!)
                error!(
                    "Failed to fetch repository updates: {:?}. Proceeding without remote URL.",
                    fetch_err
                );
                // remote_url remains None
            }
        }
    } else {
        info!("Found remote URL without needing to fetch.");
    }

    let git_info = GitInformation {
        repo_name: repo_root.file_name().unwrap().to_string_lossy().to_string(),
        commit: commit_oid.to_string(),
        remote,
        branch,
        subdir: rel_path_from_root,
    };

    Ok((git_info, repo_root.to_path_buf()))
}

pub fn run_flux_in_dir(directory: &Path, commit_hash: &str) -> Result<Vec<ErrorAndFixes>> {
    let canonical_directory = directory.canonicalize().with_context(|| {
        format!(
            "Failed to canonicalize directory path: {}",
            directory.display()
        )
    })?;

    // --- Run the main command ---
    info!(
        "Running command: cargo flux --message-format=json in {}",
        canonical_directory.display()
    );
    let command_desc = "cargo flux";
    let mut cmd = Command::new("cargo");
    cmd.args(["flux", "--message-format=json"])
        // NOTE: I think we only need this for evaluating
        // the output of new flux errors, not for diagnosing existing errors.
        // .env("FLUXFLAGS", "-Fdebug-binder-output")
        .current_dir(&canonical_directory)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());

    let mut child = cmd
        .spawn()
        .with_context(|| format!("Failed to spawn command: {}", command_desc))?;

    let stdout_handle = child.stdout.take().ok_or_else(|| {
        anyhow::anyhow!("Failed to get stdout handle for command: {}", command_desc)
    })?;

    let reader = BufReader::new(stdout_handle);
    let mut errors = Vec::new();
    let mut error_name_numbers: HashMap<String, usize> = HashMap::new();
    // Build a Vec<ErrorAndFix> from the output (the fix is, of course, empty).
    for line_result in reader.lines() {
        let line = line_result.with_context(|| {
            format!("Failed to read line from command stdout: {}", command_desc)
        })?;
        let json: serde_json::Value = serde_json::from_str(&line)?;
        // Does it have a "reason" field and is that reason "compiler-message"
        if !json
            .as_object()
            .and_then(|obj| {
                obj.get("reason")
                    .and_then(|reason| reason.as_str().map(|r| r == "compiler-message"))
            })
            .unwrap_or(false)
        {
            info!("Skipping line because it doesn't match the expected structure of a Rust compiler message: {}", line);
            continue;
        }
        let error: CompilerMessage = serde_json::from_str(&line)?;
        if error.message.level != "error" {
            continue;
        }
        let mut error_lines: VecDeque<_> = error
            .message
            .spans
            .iter()
            .flat_map(|span| span.to_line_locs(None))
            .collect();
        error_lines.extend(error.message.children.iter().flat_map(|child| {
            // NOTE: this diagnostic is apparently allowed to be recursive, but
            // I sort of doubt it in practice is ever. So I am not recurring.
            child.spans.iter().flat_map(|span| span.to_line_locs(None))
        }));
        if let Some(first_error) = error_lines.front() {
            let containing_fn_name =
                // NOTE: these files are kept relative (so that when we save
                // them, we can find the right file again), so we need to make
                // it absolute to look up the name.
                extract_function_name(&canonical_directory.join(&first_error.file), first_error.line)?.unwrap();
            let short_hash = commit_hash[..7].to_string();
            let error_name = format!(
                "{}-{}-L{}",
                short_hash, containing_fn_name, first_error.line
            );
            let error_name_entry = error_name_numbers
                .entry(error_name.clone())
                // If there is already an error name tracked, update the number
                .and_modify(|n| *n += 1)
                // Otherwise, this is the first
                .or_insert(1);
            let full_error_name = format!("{}-{}", error_name, error_name_entry);
            errors.push(ErrorAndFixes {
                error_name: full_error_name,
                error,
                // No fixes yet
                fixes: vec![],
                // NOTE: The paths in these must be relative
                error_lines,
            });
        }
    }

    child
        .wait()
        .with_context(|| format!("Failed to wait for command process: {}", command_desc))?;

    // We expect flux to fail, so we won't bother to deal with this.
    // if !status.success() {
    //     bail!("Command failed with status {}: {}", status, command_desc);
    // }
    info!("Command ran: {}", command_desc);

    Ok(errors)
}

// Compile the regex lazily and globally (or within the function scope if preferred)
// Using Lazy ensures it's compiled only once, safely across threads.
static FUNC_DEF_PATTERN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"^\s*(?:pub(?:\(.*\))?\s+)?(?:async\s+)?(?:unsafe\s+)?fn\s+([a-zA-Z_][a-zA-Z0-9_]*)",
    )
    .expect("Failed to compile function definition regex")
    // Adjusted regex slightly:
    // - Added optional `async` keyword.
    // - Added optional `pub(...)` visibility specifier.
    // - Kept the core fn name capture group `([a-zA-Z_][a-zA-Z0-9_]*)`.
});

/// Extracts the function name from the specified file at the given error line.
///
/// Args:
/// * `file_path`: Path to the Rust source file.
/// * `error_line`: The 1-based line number where the error occurred.
///
/// Returns:
/// * `Ok(Some(String))` if the function name is found.
/// * `Ok(None)` if the error line is not inside a recognizable function.
/// * `Err(e)` if there's an error reading the file or other issue.
pub fn extract_function_name(
    file_path: &Path,
    error_line: usize, // Expect 1-based line number
) -> Result<Option<String>> {
    if !file_path.is_file() {
        // Using format! to create a String error message, then boxing it.
        bail!("Cannot find file for context extraction: {:?}", file_path);
    }

    // Read all lines into memory. For very large files, line-by-line processing
    // might be better, but this matches the Python approach.
    let file = File::open(file_path)?;
    let reader = BufReader::new(file);
    let lines: Vec<String> = reader.lines().collect::<Result<_, _>>()?;

    // Ensure error_line is within bounds (adjusting for 0-based index)
    if error_line == 0 || error_line > lines.len() {
        // Or return Ok(None) if an out-of-bounds line just means no context
        bail!(
            "Error line {} is out of bounds for file {:?} ({} lines)",
            error_line,
            file_path,
            lines.len()
        );
    }

    // let mut function_start_line: Option<usize> = None; // 1-based
    let mut function_name: Option<String> = None;

    // Go backwards from the line *before* the error line (0-based index)
    // Saturating sub prevents underflow if error_line is 1.
    for i in (0..=error_line.saturating_sub(1)).rev() {
        let line = &lines[i]; // No need to trim here, regex handles leading space

        if let Some(captures) = FUNC_DEF_PATTERN.captures(line) {
            // Found the function definition line
            // function_start_line = Some(i + 1); // Store 1-based line number
            // Capture group 1 should be the function name
            if let Some(name_match) = captures.get(1) {
                function_name = Some(name_match.as_str().to_string());
            }
            break; // Stop searching backwards
        }
    }

    Ok(function_name)

    // // If we didn't find a function signature before the error line, return None
    // let (Some(fn_start_line), Some(fn_name)) = (function_start_line, function_name) else {
    //     return Ok(None);
    // };

    // // Now find the start of metadata (comments/attributes) preceding the function
    // // Default metadata start is the function start itself
    // let mut metadata_start_line = fn_start_line; // 1-based
    // // Search backwards from the line *before* the function start line (0-based index)
    // for i in (0..fn_start_line.saturating_sub(1)).rev() {
    //     let line = lines[i].trim_start(); // Trim leading whitespace only
    //     // Check for comments, attributes (#), or empty lines
    //     if line.starts_with("//") || line.starts_with('#') || line.is_empty() {
    //          // Continue searching backwards while we find metadata lines
    //          metadata_start_line = i + 1; // Update the potential start (1-based)
    //     } else {
    //         // Found a non-metadata line, the block ended on the *next* line
    //         break;
    //     }
    // }

    // // Find the end of the function using brace counting
    // let mut function_end_line: Option<usize> = None;
    // let mut brace_count: i32 = 0;
    // let mut found_opening_brace = false;

    // // Start searching from the function definition line (0-based index)
    // for i in (fn_start_line - 1)..lines.len() {
    //     let line = &lines[i];
    //     let open_braces = line.matches('{').count() as i32;
    //     let close_braces = line.matches('}').count() as i32;

    //     if !found_opening_brace && open_braces > 0 {
    //         found_opening_brace = true;
    //     }

    //     if found_opening_brace {
    //         brace_count += open_braces;
    //         brace_count -= close_braces;

    //         // If brace_count reaches 0 *after* processing this line,
    //         // and we have found at least one opening brace, this is the end line.
    //         if brace_count <= 0 {
    //              function_end_line = Some(i + 1); // Store 1-based line number
    //              break;
    //         }
    //     } else if i >= fn_start_line && close_braces > 0 {
    //         // Handle edge case: closing brace appears before the first opening brace
    //         // (e.g., malformed code or simple fn like `fn foo();`).
    //         // If we are past the fn definition line and see a '}' before '{',
    //         // assume it's not the function body we are looking for or it's malformed.
    //         // We might stop here or continue, depending on desired robustness.
    //         // For simplicity matching Python, we'll just let the loop continue,
    //         // but `found_opening_brace` prevents premature closing.
    //         // If the function has no body (like `fn foo();`), brace_count will never
    //         // become positive, and function_end_line will remain None.
    //     }
    // }

    // Ok(Some(FunctionContext {
    //     start: metadata_start_line, // Use the found metadata start (1-based)
    //     end: function_end_line,     // 1-based, or None if not found
    //     name: fn_name,
    // }))
}
