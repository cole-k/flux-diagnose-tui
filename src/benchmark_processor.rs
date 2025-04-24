use crate::benchmark_suite::BenchmarkSuite;
use crate::cached_repository::{CachedRepository, GitWorktreeDir}; // Updated import
use crate::local_paths::LocalPathResolver;
use anyhow::{Context, Result};
use clap::Parser;
use std::ffi::OsStr;
use std::fs;
use std::path::Path;

#[derive(Parser, Clone, Debug)]
pub struct BenchmarkArgs {
    /// The repos to run on (if empty, runs on all) - Matches directory names in bench_root
    #[arg(long)]
    pub repos: Vec<String>,
    /// The commits to run on (if empty, runs on all - exact match)
    #[arg(long)]
    pub commits: Vec<String>,
    /// The errors to run on (if empty, runs on all in chosen suites) - Matches error_name field
    #[arg(long)]
    pub errors: Vec<String>,
}


/// Iterates through benchmark suites based on filter criteria and executes an action.
///
/// # Arguments
/// * `benchmarks_root`: The root directory containing all benchmark suites.
/// * `filters`: Criteria to select which benchmarks to process (repo names, commits).
/// * `local_resolver`: Used to find local source code overrides.
/// * `cache_root`: The directory used for caching Git repositories.
/// * `use_cache`: Preference for using cached worktrees (true) or temporary ones (false).
/// * `action`: A closure to execute for each matching benchmark suite.
///             It receives the `BenchmarkSuite` and the `GitWorktreeDir`.
pub fn process_benchmarks<F>(
    benchmarks_root: &Path,
    filters: &BenchmarkArgs, // Pass by reference
    local_resolver: &LocalPathResolver, // Pass by reference
    cache_root: &Path,
    use_cache: bool, // Added cache preference flag
    mut action: F, // Use FnMut to allow modification of captured state if needed
) -> Result<()>
where
    F: FnMut(&BenchmarkSuite, &GitWorktreeDir) -> Result<()>, // Updated type
{
    println!(
        "Searching for benchmarks in: {:?}",
        benchmarks_root.canonicalize().unwrap_or_else(|_| benchmarks_root.to_path_buf())
    );
    println!("Applying filters: {:?}", filters);
    println!("Worktree cache preference: {}", if use_cache { "Cached" } else { "Temporary" });


    let cache_root = cache_root.canonicalize()?;
    let cache_repo = CachedRepository::new(cache_root.to_path_buf(), local_resolver);

    // 1. Iterate through repository directories
    for repo_entry_res in fs::read_dir(benchmarks_root).with_context(|| {
        format!(
            "Failed to read benchmarks root directory: {:?}",
            benchmarks_root
        )
    })? {
        let repo_entry = match repo_entry_res {
            Ok(entry) => entry,
            Err(e) => {
                eprintln!("Warning: Failed to read entry in benchmark root: {}. Skipping.", e);
                continue;
            }
        };
        let repo_path = repo_entry.path();
        if !repo_path.is_dir() {
            continue;
        }

        let repo_name = match repo_entry.file_name().into_string() {
            Ok(name) => name,
            Err(_) => {
                eprintln!("Warning: Skipping directory with non-UTF8 name: {:?}", repo_path);
                continue;
            }
        };

        // Apply repo name filter
        if !filters.repos.is_empty() && !filters.repos.contains(&repo_name) {
            continue;
        }

        // 2. Iterate through subdirectory directories (e.g., representing different projects within a repo)
        for subdir_entry_res in fs::read_dir(&repo_path)
            .with_context(|| format!("Failed to read repo directory: {:?}", repo_path))?
        {
             let subdir_entry = match subdir_entry_res {
                Ok(entry) => entry,
                Err(e) => {
                    eprintln!("Warning: Failed to read entry in repo dir {:?}: {}. Skipping.", repo_path, e);
                    continue;
                }
            };
            let subdir_path = subdir_entry.path(); // This is benchmarks_root/repo_name/subdir_name
            if !subdir_path.is_dir() {
                continue;
            }

            // We need the subdir name relative to the repo_name dir for BenchmarkSuite::new
            let subdir_name_os: &OsStr = match subdir_path.file_name() {
                 Some(name) => name,
                 None => {
                     eprintln!("Warning: Skipping path with no final component: {:?}", subdir_path);
                     continue;
                 }
             };
             let subdir_rel_path = Path::new(subdir_name_os); // Keep it as a Path for BenchmarkSuite

            // 3. Iterate through commit hash directories
            for commit_entry_res in fs::read_dir(&subdir_path)
                .with_context(|| format!("Failed to read subdir directory: {:?}", subdir_path))?
            {
                 let commit_entry = match commit_entry_res {
                    Ok(entry) => entry,
                    Err(e) => {
                        eprintln!("Warning: Failed to read entry in subdir {:?}: {}. Skipping.", subdir_path, e);
                        continue;
                    }
                };
                let commit_path = commit_entry.path(); // This is benchmarks_root/repo_name/subdir_name/commit_hash
                if !commit_path.is_dir() {
                    continue;
                }

                let commit_hash = match commit_entry.file_name().into_string() {
                    Ok(hash) => hash,
                    Err(_) => {
                        eprintln!(
                            "Warning: Skipping commit directory with non-UTF8 name: {:?}",
                            commit_path
                        );
                        continue;
                    }
                };

                // Apply commit hash filter
                if !filters.commits.is_empty() && !filters.commits.contains(&commit_hash) {
                    continue;
                }

                // 4. Found a matching suite directory, now process it
                println!(
                    "Found potential suite: {} / {} / {}",
                    repo_name,
                    subdir_name_os.to_string_lossy(),
                    commit_hash
                );

                // Create BenchmarkSuite instance
                let suite = match BenchmarkSuite::new(
                    benchmarks_root,
                    &repo_name,
                    subdir_rel_path, // Pass the relative subdir path
                    &commit_hash,
                ) {
                     Ok(s) => s,
                     Err(e) => {
                         // Log non-fatal error and skip
                         eprintln!("Warning: Failed to create BenchmarkSuite for {:?}: {}. Skipping.", commit_path, e);
                         continue;
                     }
                };

                // We NEED GitInformation to resolve the repository source code
                let git_info = match suite.git_info() {
                    Some(info) => info,
                    None => {
                        eprintln!(
                            "Warning: Missing git-info.json in suite {:?}. Cannot resolve source code. Skipping.",
                            suite.path()
                        );
                        continue;
                    }
                };

                // 5. Resolve the code repository path (local or cached, temp or persistent)
                println!("Resolving code for {}@{}...", repo_name, &commit_hash[..7]);
                let worktree = match cache_repo.get_worktree(
                    &repo_name,
                    &commit_hash,
                    &git_info.remote, // Pass remote info
                    use_cache,       // Pass cache preference
                ) {
                    Ok(wt) => wt,
                    Err(e) => {
                         // Log non-fatal error and skip
                         eprintln!(
                            "Warning: Failed to get worktree for suite {:?}: {}. Skipping.",
                            suite.path(), e
                        );
                        continue; // Skip this suite if code can't be resolved
                    }
                };
                 println!("Code worktree ready at: {:?}", worktree.path());


                // 6. Execute the provided action
                if let Err(e) = action(&suite, &worktree) {
                     // Propagate the first error encountered.
                     // Consider collecting errors if you want to process all possible suites.
                     return Err(e).with_context(|| format!("Action failed for suite {:?}", suite.path()));
                     // Or just print warning and continue:
                     // eprintln!("Warning: Action failed for suite {:?}: {}. Continuing.", suite.path(), e);
                }

                // worktree (GitWorktreeDir) automatically cleans up temporary worktrees when it goes out of scope here
                println!("Finished processing suite: {:?}.", suite.path());

            } // End commit loop
        } // End subdir loop
    } // End repo loop

    println!("Finished processing all matching benchmarks.");
    Ok(())
}
