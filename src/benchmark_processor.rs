use crate::benchmark_suite::BenchmarkSuite;
use crate::cached_repository::{CachedRepository, TempGitWorktreeDir};
use crate::local_paths::LocalPathResolver;
use anyhow::{Context, Result, bail};
use clap::Parser;
use std::fs;
use std::path::Path;

// Make BenchmarkArgs public so benchmark_processor can use it
#[derive(Parser, Clone, Debug)] // Added Debug
pub struct BenchmarkArgs {
    /// The repos to run on (if empty, runs on all)
    #[arg(long)]
    pub repos: Vec<String>,
    /// The commits to run on (if empty, runs on all - exact match)
    #[arg(long)]
    pub commits: Vec<String>,
    /// The errors to run on (if empty, runs on all in chosen suites)
    #[arg(long)]
    pub errors: Vec<String>,
    // // Moved cache_root to top-level Cli args
    // #[arg(long)]
    // cache_root: Option<PathBuf>,
}


/// Iterates through benchmark suites based on filter criteria and executes an action.
///
/// # Arguments
/// * `benchmarks_root`: The root directory containing all benchmark suites.
/// * `filters`: Criteria to select which benchmarks to process (repo names, commits).
/// * `local_resolver`: Used to find local source code overrides.
/// * `cache_root`: The directory used for caching Git repositories.
/// * `action`: A closure to execute for each matching benchmark suite.
///             It receives the `BenchmarkSuite`, the path to the temporary code worktree,
///             and the original filters (`BenchmarkArgs`) for potential inner filtering.
pub fn process_benchmarks<F>(
    benchmarks_root: &Path,
    filters: &BenchmarkArgs, // Pass by reference
    local_resolver: &LocalPathResolver, // Pass by reference
    cache_root: &Path,
    mut action: F, // Use FnMut to allow modification of captured state if needed
) -> Result<()>
where
    F: FnMut(&BenchmarkSuite, &TempGitWorktreeDir) -> Result<()>,
{
    println!(
        "Searching for benchmarks in: {:?}",
        benchmarks_root.canonicalize().unwrap_or_else(|_| benchmarks_root.to_path_buf())
    );
    println!("Applying filters: {:?}", filters);

    let cache_repo = CachedRepository::new(cache_root.to_path_buf(), local_resolver);

    // 1. Iterate through repository directories
    for repo_entry_res in fs::read_dir(benchmarks_root).with_context(|| {
        format!(
            "Failed to read benchmarks root directory: {:?}",
            benchmarks_root
        )
    })? {
        let repo_entry = repo_entry_res?;
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
            // println!("Skipping repo '{}' due to filter", repo_name); // Optional debug
            continue;
        }
        // println!("Processing repo: {}", repo_name); // Optional debug

        // 2. Iterate through subdirectory directories (directly under repo)
        //    NOTE: Current structure is repo/subdir/commit. Adjust if different.
        for subdir_entry_res in fs::read_dir(&repo_path)
            .with_context(|| format!("Failed to read repo directory: {:?}", repo_path))?
        {
            let subdir_entry = subdir_entry_res?;
            let subdir_path = subdir_entry.path(); // This is repo_name/subdir_name
            if !subdir_path.is_dir() {
                continue;
            }

            let subdir_name_os = match subdir_path.file_name() {
                Some(name) => name,
                None => {
                    eprintln!("Warning: Skipping path with no name: {:?}", subdir_path);
                    continue;
                }
            };

            // // Optional: Add subdir filtering later if needed
            // let subdir_name = subdir_name_os.to_string_lossy();
            // if !filters.subdir_names.is_empty() && !filters.subdir_names.contains(&subdir_name.to_string()) {
            //     continue;
            // }
            // println!("  Processing subdir: {}", subdir_name); // Optional debug

            // 3. Iterate through commit hash directories
            for commit_entry_res in fs::read_dir(&subdir_path)
                .with_context(|| format!("Failed to read subdir directory: {:?}", subdir_path))?
            {
                let commit_entry = commit_entry_res?;
                let commit_path = commit_entry.path(); // This is repo_name/subdir_name/commit_hash
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

                // Apply commit hash filter (simple exact match for now)
                // TODO: Implement prefix matching if desired.
                if !filters.commits.is_empty() && !filters.commits.contains(&commit_hash) {
                    // println!("    Skipping commit '{}' due to filter", commit_hash); // Optional debug
                    continue;
                }
                // println!("    Processing commit: {}", commit_hash); // Optional debug

                // 4. Found a matching suite directory, now process it
                println!(
                    "Found matching suite: {} / {} / {}",
                    repo_name,
                    subdir_name_os.to_string_lossy(),
                    commit_hash
                );

                // Create BenchmarkSuite instance (relative subdir path needed)
                let suite = match BenchmarkSuite::new(
                    benchmarks_root,
                    &repo_name,
                    Path::new(subdir_name_os), // Pass the OsStr for subdir name
                    &commit_hash,
                ) {
                     Ok(s) => s,
                     Err(e) => {
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

                // 5. Resolve the code repository path (local or cached)
                println!("Resolving code for {}@{}...", repo_name, &commit_hash[..7]);
                let worktree = match cache_repo.get_worktree(
                    &repo_name,
                    &commit_hash,
                    &git_info.remote, // Pass remote info for potential cloning/fetching
                ) {
                    Ok(guard) => guard,
                    Err(e) => {
                         eprintln!(
                            "Warning: Failed to get worktree for suite {:?}: {}. Skipping.",
                            suite.path(), e
                        );
                        continue; // Skip this suite if code can't be resolved
                    }
                };

                // 6. Execute the provided action
                if let Err(e) = action(&suite, &worktree) {
                     // Decide how to handle action errors. Propagate first error? Collect?
                     // For now, let's propagate the first error encountered.
                     bail!(
                         "Action failed for suite {:?}: {}",
                         suite.path(), e
                     );
                     // Alternatively, collect errors and return them all at the end.
                     // Or just print warning and continue:
                     // eprintln!("Warning: Action failed for suite {:?}: {}. Continuing.", suite.path(), e);
                }

                // worktree_guard automatically cleans up when it goes out of scope here
                println!("Finished processing suite: {:?}. Worktree cleaned up.", suite.path());

            } // End commit loop
        } // End subdir loop
    } // End repo loop

    println!("Finished processing all matching benchmarks.");
    Ok(())
}
