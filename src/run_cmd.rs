use anyhow::{bail, Context, Result};
use git2::{
    ErrorCode, FetchOptions, FetchPrune, Oid, ReferenceType, RemoteCallbacks, Repository, Status,
    StatusOptions,
}; // Added FetchOptions, FetchPrune, RemoteCallbacks
use log::{error, info, warn}; // Added error level
use serde::{Deserialize, Serialize};
use std::fmt;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::tui::LineLoc;
// Removed thread and Duration as we won't sleep after git2 fetch

#[derive(Debug, Clone)]
pub struct GitInformation {
    pub commit: Oid,
    pub remote: Option<String>,
    pub branch: String,
    pub rel_path_from_root: PathBuf,
    pub root_path: PathBuf,
}

impl fmt::Display for GitInformation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Commit: {}, Branch: {}, Remote: {}, Relative Path: {}",
            &self.commit.to_string()[..7],
            self.branch,
            self.remote.as_deref().unwrap_or("<none>"),
            self.rel_path_from_root.display()
        )
    }
}

/// Finds the URL of the first remote whose branch contains the given commit.
/// Iterates through local references under `refs/remotes/*`.
fn find_remote_url_containing_commit(repo: &Repository, commit_oid: Oid) -> Result<Option<String>> {
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
                                        return Ok(Some(url.to_string()));
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

pub fn run_flux_in_dir(directory: &Path) -> Result<(Vec<CompilerMessage>, GitInformation)> {
    let canonical_directory = directory.canonicalize().with_context(|| {
        format!(
            "Failed to canonicalize directory path: {}",
            directory.display()
        )
    })?;

    // --- Run the main command ---
    info!(
        "Running command: FLUX_FLAGS=\"-Fdebug-binder-output\" cargo flux --message-format=json in {}",
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
    let mut stdout_lines = Vec::new();
    for line_result in reader.lines() {
        let line = line_result.with_context(|| {
            format!("Failed to read line from command stdout: {}", command_desc)
        })?;
        let json: serde_json::Value = serde_json::from_str(&line)?;
        // Does it have a "reason" field and is that reason "compiler-message"
        if json
            .as_object()
            .and_then(|obj| {
                obj.get("reason")
                    .and_then(|reason| reason.as_str().map(|r| r == "compiler-message"))
            })
            .unwrap_or(false)
        {
            let parsed_line = serde_json::from_str(&line)?;
            stdout_lines.push(parsed_line);
        } else {
            info!("Skipping line because it doesn't match the expected structure of a Rust compiler message: {}", line);
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
    let mut remote_url = find_remote_url_containing_commit(&repo, commit_oid)?;

    if remote_url.is_none() {
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
                remote_url = find_remote_url_containing_commit(&repo, commit_oid)?;
                if remote_url.is_some() {
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
        commit: commit_oid,
        remote: remote_url, // Use the determined URL (or None)
        branch,
        rel_path_from_root,
        root_path: repo_root.to_path_buf(),
    };

    Ok((stdout_lines, git_info))
}

// --- Top Level Structure ---

#[derive(Debug, Deserialize, Serialize)]
pub struct CompilerMessage {
    pub reason: String, // e.g., "compiler-message"
    pub package_id: String,
    pub manifest_path: PathBuf, // Using PathBuf for file paths
    pub target: Target,
    pub message: Diagnostic,
}

// --- Target Structure ---

#[derive(Debug, Deserialize, Serialize)]
pub struct Target {
    pub kind: Vec<String>,        // e.g., ["dylib", "rlib"]
    pub crate_types: Vec<String>, // e.g., ["dylib", "rlib"]
    pub name: String,
    pub src_path: PathBuf, // Using PathBuf for file paths
    pub edition: String,   // e.g., "2021"
    pub doc: bool,
    pub doctest: bool,
    pub test: bool,
}

// --- Diagnostic Structure (Recursive) ---

#[derive(Debug, Deserialize, Serialize)]
pub struct Diagnostic {
    // The main message string. Often present at the top level.
    pub message: String,

    // The error/warning code, e.g., "E0999". Optional.
    pub code: Option<DiagnosticCode>,

    // Severity level, e.g., "error", "warning", "note"
    pub level: String, // Could be an Enum later if needed

    // Associated source code locations
    pub spans: Vec<RustSpan>,

    // Nested diagnostic messages (often notes or help messages)
    pub children: Vec<Diagnostic>,

    // The fully rendered message string. Often null in children.
    pub rendered: Option<String>,

    // This field seems specific to the top-level message object,
    // using rename for the '$' and Option since it might not be in children.
    #[serde(rename = "$message_type")]
    pub message_type: Option<String>, // e.g., "diagnostic"
}

// --- Diagnostic Code Structure ---

#[derive(Debug, Deserialize, Serialize)]
pub struct DiagnosticCode {
    pub code: String,                // e.g., "E0999"
    pub explanation: Option<String>, // Often null
}

// --- Span Structure ---

#[derive(Debug, Deserialize, Serialize)]
pub struct RustSpan {
    pub file_name: String,
    pub byte_start: usize,
    pub byte_end: usize,
    pub line_start: usize,
    pub line_end: usize,
    pub column_start: usize,
    pub column_end: usize,
    pub is_primary: bool,         // Is this the primary span for the diagnostic?
    pub text: Vec<TextHighlight>, // The code snippet associated with the span
    pub label: Option<String>, // Label displayed with the span, e.g., "a precondition cannot be proved"
    pub suggested_replacement: Option<String>, // Code suggestion
    pub suggestion_applicability: Option<String>, // e.g., "MachineApplicable", "HasPlaceholders", etc. (Could be Enum)
}

impl RustSpan {
    pub fn to_line_locs(&self, repo_root: &Path) -> Vec<LineLoc> {
        let mut output = Vec::with_capacity(1 + self.line_end.saturating_sub(self.line_start));
        let file = repo_root.join(Path::new(&self.file_name));
        let mut line = self.line_start;
        while line <= self.line_end {
            output.push(LineLoc::new(line, file.clone()));
            line += 1;
        }
        output
    }
}

// --- Text Highlight Structure (within Span) ---

#[derive(Debug, Deserialize, Serialize)]
pub struct TextHighlight {
    pub text: String,           // The line of code
    pub highlight_start: usize, // 1-based column index
    pub highlight_end: usize,   // 1-based column index
}
