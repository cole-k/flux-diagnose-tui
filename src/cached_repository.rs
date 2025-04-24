use crate::local_paths::LocalPathResolver;
use crate::types::RemoteInfo; // Assuming local_paths.rs is in the same crate root
use anyhow::{bail, Context, Result};
use tempfile::TempDir;
 // Using git2 crate for git operations
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use url::Url; // For sanitizing URLs for cache paths

/// Manages retrieval and caching of source code repositories.
pub struct CachedRepository<'a> {
    cache_root: PathBuf,
    local_resolver: &'a LocalPathResolver, // Borrow the resolver
}

/// Wraps a [`TempDir`] containing a worktree so that it can clean up the
/// original repo before the TempDir is dropped.
pub struct TempGitWorktreeDir {
    pub repo_path: PathBuf,
    pub worktree_dir: TempDir,
}

impl Drop for TempGitWorktreeDir {
    fn drop(&mut self) {
        println!("Cleaning up worktree: {:?}", self.worktree_dir.path());
        // Attempt to remove the worktree using the git command line
        // git2's worktree support is limited, command line is often more robust here.
        let _ = Command::new("git")
            .args(["-C", self.repo_path.to_str().unwrap()]) // Run in the main repo dir
            .args(["worktree", "remove", "--force"]) // Force removal even if dirty
            .arg(&self.worktree_dir.path())
            .status();
    }
}

impl<'a> CachedRepository<'a> {
    /// Creates a new CachedRepository manager.
    ///
    /// # Arguments
    ///
    /// * `cache_root`: The root directory for storing cached clones.
    /// * `local_resolver`: A reference to the loaded local path configuration.
    pub fn new(cache_root: PathBuf, local_resolver: &'a LocalPathResolver) -> Self {
        CachedRepository {
            cache_root,
            local_resolver,
        }
    }

    /// Gets a path to a working directory for the specified repository and commit.
    ///
    /// It follows this logic:
    /// 1. Check `LocalPathResolver` for a user-defined local path (commit-specific then default).
    /// 2. If found and valid, creates a temporary worktree *from* that local repo.
    /// 3. If not found, checks the internal cache (`cache_root`).
    /// 4. If not cached, clones the `remote_url`.
    /// 5. Fetches the specific `commit_hash` into the cached repo if needed.
    /// 6. Creates a temporary worktree *from* the cached repo.
    ///
    /// Returns a tuple containing the path to the worktree and a `WorktreeGuard`
    /// which will clean up the worktree when dropped.
    pub fn get_worktree(
        &self,
        repo_name: &str,
        commit_hash: &str,
        remote: &Option<RemoteInfo>, // Needed for cloning if not local/cached
    ) -> Result<TempGitWorktreeDir> {
        // 1. Try resolving local path first
        if let Some(local_repo_path) = self.local_resolver.resolve(repo_name, commit_hash) {
            println!("Found local path override: {:?}", local_repo_path);
            if local_repo_path.is_dir() {
                match self.validate_and_create_worktree(local_repo_path, commit_hash) {
                    Ok(result) => return Ok(result),
                    Err(e) => {
                        eprintln!(
                            "Warning: Failed to use local path override {:?}: {}. Falling back to cache.",
                            local_repo_path, e
                        );
                        // Fall through to cache logic
                    }
                }
            } else {
                eprintln!(
                    "Warning: Local path override {:?} not found or not a directory. Falling back to cache.",
                    local_repo_path
                );
                // Fall through to cache logic
            }
        }

        // 2. Use cache (clone/fetch if necessary)
        println!(
            "No valid local path found, using cache for {}@{}",
            repo_name, commit_hash
        );
        let Some(remote) = remote else {
            bail!(
                "Cannot fetch repository '{}': No remote_url provided and no local path found.",
                repo_name
            );
        };

        let cached_repo_path =
            self.calculate_cache_path(&remote.remote_url)
                .with_context(|| {
                    format!(
                        "Failed to calculate cache path for url: {}",
                        remote.remote_url
                    )
                })?;

        let repo = self
            .ensure_cached_repo(&cached_repo_path, &remote.remote_url)
            .with_context(|| {
                format!(
                    "Failed to ensure cached repository at {:?}",
                    cached_repo_path
                )
            })?;

        self.fetch_commit(&repo, commit_hash, remote)
            .with_context(|| format!("Failed to fetch commit {} in cache", commit_hash))?;

        // 3. Create worktree from cache
        self.validate_and_create_worktree(&cached_repo_path, commit_hash)
    }

    /// Ensures the repository exists in the cache, cloning if necessary.
    fn ensure_cached_repo(&self, path: &Path, remote_url: &str) -> Result<git2::Repository> {
        if path.exists() {
            println!("Cache hit: {:?}", path);
            let repo = git2::Repository::open(path)
                .with_context(|| format!("Failed to open cached repository at {:?}", path))?;
            // Optional: Verify remote URL matches? Could be complex if user manually changed it.
            Ok(repo)
        } else {
            println!("Cache miss, cloning {} into {:?}", remote_url, path);
            fs::create_dir_all(path.parent().unwrap()).with_context(|| {
                format!("Failed to create cache directory structure for {:?}", path)
            })?;

            let fo = git2::FetchOptions::new();
            // Add authentication callbacks here if needed (e.g., SSH keys)
            // fo.callbacks(...);

            let mut builder = git2::build::RepoBuilder::new();
            builder.fetch_options(fo);

            let repo = builder
                .clone(remote_url, path)
                .with_context(|| format!("Failed to clone {} into {:?}", remote_url, path))?;
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
            println!("Commit {} already exists locally.", commit_hash);
            return Ok(());
        }

        println!(
            "Commit {} not found locally, fetching from origin...",
            commit_hash
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
        let refspec = commit_hash.to_string(); // Fetch the commit directly
        match remote.fetch(&[&refspec], Some(&mut fo), None) {
            Ok(_) => {
                println!("Successfully fetched commit {} directly.", commit_hash);
                // Verify commit exists now
                if repo.find_commit(oid).is_ok() {
                    return Ok(());
                } else {
                    eprintln!("Warning: Fetched commit {} directly, but still couldn't find it. Trying full fetch.", commit_hash);
                }
            }
            Err(e) => {
                eprintln!(
                    "Warning: Failed to fetch commit {} directly: {}. Attempting full fetch.",
                    commit_hash, e
                );
            }
        }

        // Fallback: Fetch all from origin (might be needed if history is complex)
        // Reset fetch options if needed
        let mut fo_full = git2::FetchOptions::new();
        remote
            .fetch::<&str>(&[], Some(&mut fo_full), None) // Fetch all default refspecs
            .with_context(|| {
                format!(
                    "Failed to fetch from remote 'origin' ({})",
                    remote_info.remote_url
                )
            })?;

        // Final check
        repo.find_commit(oid).with_context(|| {
            format!(
                "Commit {} not found even after fetching from {}",
                commit_hash, remote_info.remote_url
            )
        })?;

        Ok(())
    }

    /// Validates a commit exists and creates a unique temporary worktree.
    fn validate_and_create_worktree(
        &self,
        repo_path: &Path,
        commit_hash: &str,
    ) -> Result<TempGitWorktreeDir> {
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

        let worktree_dir = tempfile::tempdir()?;

        println!("Creating worktree at: {:?}", worktree_dir.path());

        // Use git2's worktree add (requires libgit2 1.1+) or shell out
        // Shelling out is often more reliable across git versions
        let status = Command::new("git")
            .args(["-C", repo_path.to_str().unwrap()])
            .args(["worktree", "add", "--detach"])
            .arg(&worktree_dir.path())
            .arg(commit_hash)
            .status()
            .context("Failed to execute git worktree add command")?;

        if !status.success() {
            bail!(
                "'git worktree add' command failed for commit {} at {:?}",
                commit_hash,
                worktree_dir.path()
            );
        }

        Ok(TempGitWorktreeDir {
            repo_path: repo_path.to_path_buf(),
            worktree_dir,
        })
    }

    /// Creates a safe directory name from a URL.
    pub fn calculate_cache_path(&self, remote_url: &str) -> Result<PathBuf> {
        let parsed_url = Url::parse(remote_url)
            .with_context(|| format!("Failed to parse remote URL: {}", remote_url))?;
        let host = parsed_url.host_str().unwrap_or("local");
        let path = parsed_url.path().trim_start_matches('/').replace('/', "_");
        // Basic sanitization - replace non-alphanumeric with underscore
        let safe_host = host
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '.' {
                    c
                } else {
                    '_'
                }
            })
            .collect::<String>();
        let safe_path = path
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '_' || c == '.' {
                    c
                } else {
                    '_'
                }
            })
            .collect::<String>();
        // Remove trailing .git if present
        let safe_path = safe_path
            .strip_suffix("_git")
            .unwrap_or(&safe_path)
            .to_string();

        Ok(self
            .cache_root
            .join("repos")
            .join(safe_host)
            .join(safe_path))
    }
}
