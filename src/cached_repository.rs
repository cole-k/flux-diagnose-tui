use crate::local_paths::LocalPathResolver;
use crate::types::RemoteInfo;
use anyhow::{bail, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;
use url::Url; // For sanitizing URLs for cache paths

/// Manages retrieval and caching of source code repositories and worktrees.
pub struct CachedRepository<'a> {
    cache_root: PathBuf,
    local_resolver: &'a LocalPathResolver, // Borrow the resolver
}

/// Represents a Git worktree, which can be temporary (cleaned up on drop)
/// or cached (persists on disk).
#[derive(Debug)]
pub struct GitWorktreeDir {
    /// Path to the main bare/cloned repo from which the worktree was created.
    /// For worktrees derived from local overrides, this points to the user's local repository path.
    pub repo_path: PathBuf,
    /// Path to the checked-out worktree directory (either temporary or cached).
    pub worktree_path: PathBuf,
    /// If Some(_), this is a temporary worktree managed by TempDir.
    /// The TempDir handle ensures the directory is cleaned up when this struct is dropped.
    _temporary_marker: Option<TempDir>,
}

impl GitWorktreeDir {
    /// Returns the path to the worktree directory.
    pub fn path(&self) -> &Path {
        &self.worktree_path
    }
}

impl Drop for GitWorktreeDir {
    fn drop(&mut self) {
        // Only run `git worktree remove` if it's a temporary worktree.
        // The TempDir's own Drop impl handles deleting the directory itself.
        if let Some(temp_dir) = self._temporary_marker.take() {
            // Using take() correctly consumes the Option<TempDir>
            println!("Cleaning up temporary worktree: {:?}", temp_dir.path());
            // Attempt to remove the worktree using the git command line
            // Run the command relative to the *original* repository path
            let repo_path_str = match self.repo_path.to_str() {
                 Some(s) => s,
                 None => {
                     eprintln!("Warning: Cannot convert repository path {:?} to string for cleanup.", self.repo_path);
                     // temp_dir is dropped here, cleaning up the actual directory on disk.
                     return;
                 }
            };
            let status = Command::new("git")
                .args(["-C", repo_path_str]) // Run in the main repo dir (bare or local)
                .args(["worktree", "remove", "--force"]) // Force removal even if dirty
                .arg(&self.worktree_path) // Use the stored worktree path
                .status();

            match status {
                Ok(s) if s.success() => {
                    println!("Successfully removed temporary worktree entry for: {:?}", temp_dir.path());
                }
                Ok(s) => {
                     eprintln!(
                        "Warning: 'git worktree remove' failed with status {} for temporary worktree {:?} (repo {:?}). The directory might still be cleaned up.",
                        s, temp_dir.path(), self.repo_path
                     );
                }
                Err(e) => {
                     eprintln!(
                        "Warning: Failed to execute 'git worktree remove' for temporary worktree {:?} (repo {:?}): {}. The directory might still be cleaned up.",
                        temp_dir.path(), self.repo_path, e
                     );
                }
            }
            // temp_dir is dropped here, cleaning up the actual directory on disk.
        } else {
            // This is a cached worktree, do nothing on drop.
            println!("Skipping cleanup for cached worktree: {:?}", self.worktree_path);
        }
    }
}

impl<'a> CachedRepository<'a> {
    /// Creates a new CachedRepository manager.
    ///
    /// # Arguments
    ///
    /// * `cache_root`: The root directory for storing cached clones and worktrees.
    /// * `local_resolver`: A reference to the loaded local path configuration.
    pub fn new(cache_root: PathBuf, local_resolver: &'a LocalPathResolver) -> Self {
        CachedRepository {
            cache_root,
            local_resolver,
        }
    }

    /// Gets a path to a working directory for the specified repository and commit.
    ///
    /// Behavior depends on `use_cache`:
    /// - **Local Override:** If a local path override is found via `local_resolver`:
    ///   - If `use_cache` is `true`:
    ///     1. Checks for an existing *cached worktree* specifically for this local override
    ///        (using `local-worktrees/repo-name/commit` path). If valid, returns it.
    ///     2. If not found/invalid, creates a new *cached worktree* from the *local* repository path.
    ///   - If `use_cache` is `false`:
    ///     1. Creates a *temporary worktree* from the *local* repository path.
    /// - **No Local Override:**
    ///   - If `use_cache` is `true` (e.g., for `eval`):
    ///     1. Checks for an existing *cached worktree* for the remote commit. If valid, returns it.
    ///     2. If not found/invalid, ensures the *cached bare repo* exists (cloning/fetching if needed).
    ///     3. Creates a new *cached worktree* from the bare repo and returns it.
    ///   - If `use_cache` is `false` (e.g., for `add`):
    ///     1. Ensures the *cached bare repo* exists (cloning/fetching if needed).
    ///     2. Creates a *temporary worktree* from the bare repo and returns it.
    ///
    /// # Arguments
    /// * `repo_name`: Logical name of the repository (used for local path resolution and cache paths).
    /// * `commit_hash`: The specific commit hash to check out.
    /// * `remote`: Optional remote information (URL, name) needed for cloning/fetching if no local override.
    /// * `use_cache`: If true, prefers using/creating a persistent cached worktree.
    ///                If false, creates a temporary worktree.
    ///
    /// # Returns
    /// A `Result` containing `GitWorktreeDir` which manages the worktree lifetime.
    pub fn get_worktree(
        &self,
        repo_name: &str,
        commit_hash: &str,
        remote: &Option<RemoteInfo>,
        use_cache: bool,
    ) -> Result<GitWorktreeDir> {
        // 1. Try resolving local path first
        if let Some(local_repo_path) = self.local_resolver.resolve(repo_name, commit_hash) {
            println!(
                "Found local path override: {:?}. Cache preference: {}",
                local_repo_path,
                if use_cache { "Cached" } else { "Temporary" }
            );

            // Ensure the local path exists and is a directory before proceeding
            if !local_repo_path.is_dir() {
                eprintln!(
                    "Warning: Local path override {:?} not found or not a directory. Falling back to cache/remote.",
                    local_repo_path
                );
                // Fall through to the remote/cache logic below
            } else {
                // FIXME: We should always do the cache lookup, regardless of use_cache.
                // This parameter is only for determining whether to _save_ to a cache.
                //
                // Local path exists, decide whether to cache or use temporary based on use_cache
                if use_cache {
                    // Attempt to create or get a *cached* worktree from the *local* repo
                    println!("Attempting to use cached worktree for local override.");
                    // FIXME: This should be two separate functions (get and create) and it
                    // should be independent of whether it is local or remote. We only need to
                    // use the local or remote information to determine the path.
                    let worktree_path = self.calculate_local_cached_worktree_path(repo_name, commit_hash)?;
                    return self.create_or_get_cached_worktree(local_repo_path, &worktree_path, commit_hash)
                        .with_context(|| format!("Failed to get/create cached worktree from local override path: {:?}", local_repo_path));
                } else {
                    // Create a temporary worktree from the local repo (existing behavior for use_cache=false)
                    println!("Creating temporary worktree for local override.");
                    return self.create_temporary_worktree(local_repo_path, commit_hash)
                        .with_context(|| format!("Failed to create temporary worktree from local override path: {:?}", local_repo_path));
                }
            }
        }

        // 2. No valid local override, proceed with cache/remote logic
        println!(
            "Using cache/remote for {}@{}. Cache preference: {}",
            repo_name,
            &commit_hash[..7],
            if use_cache { "Cached" } else { "Temporary" }
        );
        let Some(remote) = remote else {
            bail!(
                "Cannot fetch repository '{}': No remote_url provided in git-info.json and no local path override found.",
                repo_name
            );
        };

        let cached_repo_path = self.ensure_repo_path_exists(&remote.remote_url)?;
        let repo = self.ensure_cached_repo(&cached_repo_path, &remote.remote_url)?;
        self.fetch_commit(&repo, commit_hash, remote)?;

        // 3. Create worktree (Cached or Temporary) based on use_cache flag for remote repo
        // FIXME: We should always do the cache lookup, regardless of use_cache.
        // This parameter is only for determining whether to _save_ to a cache.
        //
        if use_cache {
            let worktree_path = self.calculate_remote_cached_worktree_path(&remote.remote_url, commit_hash)?.canonicalize()?;
            self.create_or_get_cached_worktree(&cached_repo_path, &worktree_path, commit_hash)
                .with_context(|| format!("Failed to get/create cached worktree for remote commit {}", commit_hash))
        } else {
            self.create_temporary_worktree(&cached_repo_path, commit_hash)
                 .with_context(|| format!("Failed to create temporary worktree for remote commit {}", commit_hash))
        }
    }

    /// Ensures the base directory for the cached repository exists.
    /// Does *not* perform the clone.
    fn ensure_repo_path_exists(&self, remote_url: &str) -> Result<PathBuf> {
        let cached_repo_path = self.calculate_remote_repo_cache_path(remote_url).with_context(|| {
            format!("Failed to calculate cache path for url: {}", remote_url)
        })?;

        if !cached_repo_path.exists() {
             fs::create_dir_all(cached_repo_path.parent().unwrap()).with_context(|| {
                format!("Failed to create cache directory structure for {:?}", cached_repo_path)
            })?;
        }
        Ok(cached_repo_path)
    }


    /// Ensures the repository exists in the cache, cloning if necessary.
    fn ensure_cached_repo(&self, path: &Path, remote_url: &str) -> Result<git2::Repository> {
        if path.join(".git").exists() || path.join("HEAD").exists() { // Check if it looks like a git repo (bare or non-bare)
            println!("Cache hit: Found repository at {:?}", path);
            let repo = git2::Repository::open(path)
                .with_context(|| format!("Failed to open cached repository at {:?}", path))?;
            // Optional: Add verification logic here (e.g., check remote URL) if needed
            Ok(repo)
        } else {
            println!("Cache miss: Cloning {} into {:?}", remote_url, path);
            // Parent directory should exist from ensure_repo_path_exists

            // Use git2 for cloning
            let fo = git2::FetchOptions::new();
            // Add authentication callbacks here if needed (e.g., SSH keys)
            // fo.callbacks(...);

            let mut builder = git2::build::RepoBuilder::new();
            builder.fetch_options(fo);
            builder.bare(true); // Clone as bare repo

            let repo = builder
                .clone(remote_url, path)
                .with_context(|| format!("Failed to clone {} as bare repo into {:?}", remote_url, path))?;
            Ok(repo)
        }
    }

    /// Fetches the specific commit if it's not present in the repository.
    fn fetch_commit(
        &self,
        repo: &git2::Repository,
        commit_hash: &str,
        remote_info: &RemoteInfo,
    ) -> Result<()> {
        let oid = git2::Oid::from_str(commit_hash)
            .with_context(|| format!("Invalid commit hash format: {}", commit_hash))?;

        // Check if commit exists locally
        if repo.find_commit(oid).is_ok() {
            println!("Commit {} already exists locally.", &commit_hash[..7]);
            return Ok(());
        }

        println!(
            "Commit {} not found locally, fetching from '{}' ({})",
            &commit_hash[..7], remote_info.remote_name, remote_info.remote_url
        );
        let mut remote = repo
            .find_remote(&remote_info.remote_name)
            .or_else(|_| repo.remote(&remote_info.remote_name, &remote_info.remote_url)) // Add remote if it doesn't exist
            .with_context(|| {
                format!(
                    "Failed to find or add remote '{}' ({})",
                    remote_info.remote_name, remote_info.remote_url
                )
            })?;

        let mut fo = git2::FetchOptions::new();
        // Add authentication callbacks here if needed
        // fo.callbacks(...);

        // Try fetching the specific commit refspec first (might be faster)
        let refspec = format!("+refs/heads/*:refs/remotes/{}/*", remote_info.remote_name);
        let refspec_commit = commit_hash.to_string(); // Also try fetching the commit SHA directly

        // Fetch default branches and the specific commit
        match remote.fetch(&[&refspec, &refspec_commit], Some(&mut fo), None) {
            Ok(_) => {
                println!("Successfully fetched refspecs including potential commit {}.", &commit_hash[..7]);
                // Verify commit exists now
                if repo.find_commit(oid).is_ok() {
                    return Ok(());
                } else {
                    eprintln!("Warning: Fetch seemed successful, but still couldn't find commit {}. Possible issue with remote state or refspec.", &commit_hash[..7]);
                    // Proceed to final check, maybe it arrived via default fetch anyway
                }
            }
            Err(e) => {
                eprintln!(
                    "Warning: Failed to fetch directly for commit {}: {}. Relying on existing data or previous fetches.",
                     &commit_hash[..7], e
                );
                 // Don't necessarily fail here, the commit might exist from a prior full fetch
            }
        }

        // Final check after fetch attempt
        repo.find_commit(oid).with_context(|| {
            format!(
                "Commit {} not found in {:?} even after attempting fetch from {}",
                commit_hash, repo.path(), remote_info.remote_url
            )
        })?;

        Ok(())
    }

    /// Validates commit exists and creates a temporary worktree from the given repo_path.
    /// This is used for both local overrides (when use_cache=false) and remote repos (when use_cache=false).
    fn create_temporary_worktree(
        &self,
        repo_path: &Path, // Path to the source repo (local override or cached bare repo)
        commit_hash: &str,
    ) -> Result<GitWorktreeDir> {
        let repo = git2::Repository::open(repo_path)
            .with_context(|| format!("Failed to open repository at {:?}", repo_path))?;

        let oid = git2::Oid::from_str(commit_hash)
            .with_context(|| format!("Invalid commit hash format: {}", commit_hash))?;

        // Ensure commit exists in this repo
        repo.find_commit(oid).with_context(|| {
            format!(
                "Commit {} not found in repository {:?}",
                commit_hash, repo_path
            )
        })?;

        let temp_dir = tempfile::tempdir()?;
        let worktree_path = temp_dir.path().to_path_buf();

        println!("Creating temporary worktree at: {:?}", worktree_path);

        // Use shell out for robustness, run relative to the source repo path
        let repo_path_str = repo_path.to_str().ok_or_else(|| anyhow::anyhow!("Invalid repo path string: {:?}", repo_path))?;
        let status = Command::new("git")
            .args(["-C", repo_path_str])
            .args(["worktree", "add", "--detach"])
            .arg(&worktree_path)
            .arg(commit_hash)
            .status()
            .context("Failed to execute git worktree add command for temporary worktree")?;

        if !status.success() {
            bail!(
                "'git worktree add' command failed for temporary worktree (commit {}) at {:?} from repo {:?}",
                commit_hash,
                worktree_path,
                repo_path
            );
        }

        Ok(GitWorktreeDir {
            repo_path: repo_path.to_path_buf(), // Store the path of the repo it came from
            worktree_path,
            _temporary_marker: Some(temp_dir), // Mark as temporary
        })
    }

    fn lookup_cached_worktree(
        &self,
        repo_path: &Path, // Path to the *cached bare* repo
        worktree_path: &Path,
    ) -> Option<GitWorktreeDir> {
        // Check if a valid worktree already exists
        // A valid git worktree contains a `.git` file pointing to the main repo.
        let git_file_path = worktree_path.join(".git");
        if worktree_path.is_dir() && git_file_path.is_file() {
             // Optional: Add more validation, e.g., read .git file, run `git worktree list`
             println!("Found existing cached remote worktree at: {:?}", worktree_path);
             Some(GitWorktreeDir {
                 repo_path: repo_path.to_path_buf(), // Points to the cached bare repo
                 worktree_path: worktree_path.to_path_buf(),
                 _temporary_marker: None, // Mark as cached
             })
        } else {
            None
        }
    }

    /// Gets path to existing cached worktree or creates a new one from the cached *remote* bare repo.
    fn create_or_get_cached_worktree(
        &self,
        repo_path: &Path, // Path to the *cached bare* repo
        worktree_path: &Path,
        commit_hash: &str,
    ) -> Result<GitWorktreeDir> {
        if let Some(found_worktree) = self.lookup_cached_worktree(repo_path, &worktree_path) {
            return Ok(found_worktree);
        }
        let repo = git2::Repository::open(repo_path)
            .with_context(|| format!("Failed to open repository at {:?}", repo_path))?;

        let oid = git2::Oid::from_str(commit_hash)
            .with_context(|| format!("Invalid commit hash format: {}", commit_hash))?;

        // Ensure commit exists in this repo (might be redundant if called after fetch_commit, but safe)
        repo.find_commit(oid).with_context(|| {
            format!(
                "Commit {} not found in repository {:?} before creating cached worktree",
                commit_hash, repo_path
            )
        })?;

        // Check if a valid worktree already exists
        // A valid git worktree contains a `.git` file pointing to the main repo.
        let git_file_path = worktree_path.join(".git");
        if worktree_path.is_dir() && git_file_path.is_file() {
             // Optional: Add more validation, e.g., read .git file, run `git worktree list`
             println!("Found existing cached remote worktree at: {:?}", worktree_path);
             return Ok(GitWorktreeDir {
                 repo_path: repo_path.to_path_buf(), // Points to the cached bare repo
                 worktree_path: worktree_path.to_path_buf(),
                 _temporary_marker: None, // Mark as cached
             });
        }

        if worktree_path.exists() {
             bail!("FATAL: Path {:?} exists but is not a valid Git worktree directory.", worktree_path);
        }


        println!("Creating new cached remote worktree at: {:?}", worktree_path);
        fs::create_dir_all(&worktree_path).with_context(|| format!("Failed to create directory for cached worktree: {:?}", worktree_path))?;


        // Use shell out for robustness, relative to the cached bare repo
        let repo_path_str = repo_path.to_str().ok_or_else(|| anyhow::anyhow!("Invalid repo path string: {:?}", repo_path))?;
        let status = Command::new("git")
            .args(["-C", repo_path_str])
            .args(["worktree", "add", "--detach"])
            .arg(&worktree_path)
            .arg(commit_hash)
            .status()
            .context("Failed to execute git worktree add command for cached remote worktree")?;

        if !status.success() {
            // Clean up the created directory if 'git worktree add' failed
            fs::remove_dir_all(&worktree_path)?;
            bail!(
                "'git worktree add' command failed for cached remote worktree (commit {}) at {:?} from repo {:?}",
                commit_hash,
                worktree_path,
                repo_path
            );
        }

         Ok(GitWorktreeDir {
            repo_path: repo_path.to_path_buf(), // Points to the cached bare repo
            worktree_path: worktree_path.to_path_buf(),
            _temporary_marker: None, // Mark as cached
        })
    }

    /// Calculates the cache path for the bare repository clone based on remote URL.
    /// Ensures it's under a `repos` subdirectory.
    pub fn calculate_remote_repo_cache_path(&self, remote_url: &str) -> Result<PathBuf> {
        let (safe_host, safe_path) = self.sanitize_url_for_path(remote_url)?;
        Ok(self
            .cache_root
            .join("repos") // Store bare clones under 'repos'
            .join(safe_host)
            .join(safe_path))
    }

    /// Calculates the cache path for a persistent worktree derived from a remote repo.
    /// Ensures it's under a `worktrees/remote` subdirectory structure.
    fn calculate_remote_cached_worktree_path(&self, remote_url: &str, commit_hash: &str) -> Result<PathBuf> {
        let (safe_host, safe_repo_name) = self.sanitize_url_for_path(remote_url)?;
        let safe_commit = self.sanitize_commit_hash_for_path(commit_hash)?;

        Ok(self
            .cache_root
            .join("worktrees") // Store worktrees under 'worktrees'
            .join("remote")    // Subdir for remote-derived worktrees
            .join(safe_host)
            .join(safe_repo_name)
            .join(safe_commit)) // Use sanitized commit hash as final dir name
    }

     /// Calculates the cache path for a persistent worktree derived from a local override.
    /// Ensures it's under a `worktrees/local` subdirectory structure.
    fn calculate_local_cached_worktree_path(&self, repo_name: &str, commit_hash: &str) -> Result<PathBuf> {
        // Sanitize repo_name (basic sanitization)
        let safe_repo_name = repo_name
            .chars()
            .map(|c| match c {
                '/' | '\\' | ':' | '.' | ' ' => '_', // Replace common problematic chars + space
                _ if c.is_alphanumeric() || c == '-' || c == '_' => c, // Allow alphanumeric, dash, underscore
                _ => '_', // Replace others with underscore
            })
            .collect::<String>();

        let safe_commit = self.sanitize_commit_hash_for_path(commit_hash)?;

        Ok(self
            .cache_root
            .join("worktrees")      // Store worktrees under 'worktrees'
            .join("local")          // Subdir for local-derived worktrees
            .join(safe_repo_name)   // Use sanitized repo name
            .join(safe_commit))     // Use sanitized commit hash
    }

    /// Sanitizes a commit hash string for safe use in a directory path.
    fn sanitize_commit_hash_for_path(&self, commit_hash: &str) -> Result<String> {
        let safe_commit = commit_hash
            .chars()
            .filter(|c| c.is_alphanumeric())
            .collect::<String>();
        if safe_commit.len() < 7 { // Ensure it's not completely invalid (e.g., empty or too short)
            bail!("Invalid commit hash for path generation: {}", commit_hash);
        }
        Ok(safe_commit)
    }

     /// Creates safe directory names from a URL (host and path components).
     fn sanitize_url_for_path(&self, remote_url: &str) -> Result<(String, String)> {
        let parsed_url = Url::parse(remote_url)
            .with_context(|| format!("Failed to parse remote URL: {}", remote_url))?;
        let host = parsed_url.host_str().unwrap_or("local_host"); // Provide default for local paths
        let path = parsed_url.path().trim_start_matches('/');

        // Sanitize host: replace non-alphanumeric/dot with underscore
        let safe_host = host
            .chars()
            .map(|c| if c.is_alphanumeric() || c == '.' { c } else { '_' })
            .collect::<String>();

        // Sanitize path: replace slashes, and non-alphanumeric/dot/underscore with underscore
        let safe_path_intermediate = path.replace('/', "_");
        let mut safe_path = safe_path_intermediate
            .chars()
            .map(|c| if c.is_alphanumeric() || c == '.' || c == '_' { c } else { '_' })
            .collect::<String>();

        // Remove trailing .git or _git
        if safe_path.ends_with("_git") {
            safe_path.truncate(safe_path.len() - 4);
        } else if safe_path.ends_with(".git") {
             safe_path.truncate(safe_path.len() - 4);
        }

        // Handle empty path case (e.g., URL is just hostname)
        let safe_path = if safe_path.is_empty() { "_repo".to_string() } else { safe_path };


        Ok((safe_host, safe_path))
    }
}
