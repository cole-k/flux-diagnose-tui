use crate::local_paths::LocalPathResolver; // Assuming local_paths.rs is in the same crate root
use crate::types::{ErrorAndFixes, GitInformation}; // Assuming types.rs is in crate root
use anyhow::{anyhow, Context, Result};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Represents the benchmark definitions for a single commit of a repository.
#[derive(Debug)]
pub struct BenchmarkSuite {
    suite_path: PathBuf, // Path to repo_name/subdir_name/commit_hash/
    repo_name: String,
    subdir_name: String,
    commit_hash: String,
    git_info: Option<GitInformation>, // Loaded from git-info.json if it exists
}

impl BenchmarkSuite {
    /// Creates or loads a BenchmarkSuite instance.
    ///
    /// It determines the path but doesn't read benchmark files until explicitly asked.
    /// It tries to load `git-info.json` if it exists.
    ///
    /// # Arguments
    ///
    /// * `benchmarks_root`: The root directory containing all benchmark suites (e.g., `./benchmarks`).
    /// * `repo_name`: The logical name of the repository.
    /// * `subdir_name`: The name of the subdirectory in the repo.
    /// * `commit_hash`: The full commit hash string.
    pub fn new(
        benchmarks_root: &Path,
        repo_name: &str,
        subdir: &Path,
        commit_hash: &str,
    ) -> Result<Self> {
        let subdir_name = subdir
            .file_name()
            .ok_or_else(|| anyhow!("Cannot get name of Subdirectory {:?}", subdir))?
            .to_string_lossy();
        let suite_path = benchmarks_root
            .join(repo_name)
            .join(subdir)
            .join(commit_hash);
        let git_info_path = suite_path.join("git-info.json");

        let git_info = if git_info_path.exists() {
            let content = fs::read_to_string(&git_info_path).with_context(|| {
                format!(
                    "Failed to read existing git-info.json at {:?}",
                    git_info_path
                )
            })?;
            Some(serde_json::from_str(&content).with_context(|| {
                format!(
                    "Failed to parse existing git-info.json at {:?}",
                    git_info_path
                )
            })?)
        } else {
            None
        };

        Ok(Self {
            suite_path,
            repo_name: repo_name.to_string(),
            subdir_name: subdir_name.to_string(),
            commit_hash: commit_hash.to_string(),
            git_info,
        })
    }

    /// Returns the path to this suite's directory.
    pub fn path(&self) -> &Path {
        &self.suite_path
    }

    /// Returns the loaded GitInfoFile, if it was found when creating the suite.
    pub fn git_info(&self) -> Option<&GitInformation> {
        self.git_info.as_ref()
    }

    /// Loads all benchmark definitions (`*.json` files, excluding `git-info.json`)
    /// from the suite's directory.
    ///
    /// Returns an empty Vec if the directory doesn't exist or contains no benchmark files.
    pub fn load_benchmarks(&self) -> Result<Vec<ErrorAndFixes>> {
        let mut benchmarks = Vec::new();
        if !self.suite_path.exists() {
            return Ok(benchmarks); // Not an error, just no benchmarks yet
        }

        let entries = fs::read_dir(&self.suite_path).with_context(|| {
            format!(
                "Failed to read benchmark suite directory: {:?}",
                self.suite_path
            )
        })?;

        for entry_res in entries {
            let entry = entry_res?;
            let path = entry.path();

            if path.is_file()
                && path.extension().is_some_and(|ext| ext == "json")
                && path
                    .file_name().is_some_and(|name| name != "git-info.json")
            {
                let content = fs::read_to_string(&path)
                    .with_context(|| format!("Failed to read benchmark file: {:?}", path))?;
                let benchmark: ErrorAndFixes = serde_json::from_str(&content)
                    .with_context(|| format!("Failed to parse benchmark file: {:?}", path))?;
                benchmarks.push(benchmark);
            }
        }
        Ok(benchmarks)
    }

    /// Loads a single benchmark definition by its error name.
    pub fn load_single_benchmark(&self, error_name: &str) -> Result<Option<ErrorAndFixes>> {
        let filename = format!("{}.json", error_name);
        let file_path = self.suite_path.join(&filename);

        if !file_path.exists() {
            return Ok(None);
        }

        let content = fs::read_to_string(&file_path)
            .with_context(|| format!("Failed to read benchmark file: {:?}", file_path))?;
        let benchmark: ErrorAndFixes = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse benchmark file: {:?}", file_path))?;
        Ok(Some(benchmark))
    }

    /// Writes the complete set of benchmark definitions to the suite directory,
    /// overwriting existing files.
    ///
    /// This function also creates/updates `git-info.json` based on the provided
    /// `GitInformation` and updates the `.localpaths.toml` using the `LocalPathResolver`.
    ///
    /// # Arguments
    ///
    /// * `benchmarks`: The full list of `ErrorAndFixes` to write for this suite.
    /// * `git_info`: The *current* GitInformation reflecting how this run was performed.
    /// * `local_resolver`: The resolver used to record the local path override.
    pub fn write_benchmarks(
        &mut self, // Needs mut to update self.git_info
        benchmarks: &[ErrorAndFixes],
        git_info: &GitInformation, // Explicitly name it for clarity
        local_resolver: &LocalPathResolver, // Pass by reference
        repo_path: &Path,
    ) -> Result<()> {
        // 1. Ensure directory exists
        fs::create_dir_all(&self.suite_path)
            .with_context(|| format!("Failed to create suite directory: {:?}", self.suite_path))?;

        // 2. Create/Update git-info.json
        let git_info_path = self.suite_path.join("git-info.json");
        let git_info_json =
            serde_json::to_string_pretty(&git_info).context("Failed to serialize git-info.json")?;
        fs::write(&git_info_path, git_info_json)
            .with_context(|| format!("Failed to write git-info.json to {:?}", git_info_path))?;
        // Update the cached version in self
        self.git_info = Some(git_info.clone());

        // 3. Write benchmark files (Consider removing old ones first?)
        // Simplest approach: just write, overwriting existing ones.
        // More robust: List existing *.json, remove ones not in the new list, then write.
        // Let's stick to overwriting for now.
        for benchmark in benchmarks {
            let filename = format!("{}.json", benchmark.error_name); // Add sanitization if needed
            let error_path = self.suite_path.join(filename);
            let error_json = serde_json::to_string_pretty(benchmark).with_context(|| {
                format!("Failed to serialize benchmark: {}", benchmark.error_name)
            })?;
            fs::write(&error_path, error_json)
                .with_context(|| format!("Failed to write benchmark file to {:?}", error_path))?;
        }

        // Optional: Clean up stale benchmark files not present in the `benchmarks` list.
        // self.cleanup_stale_files(benchmarks)?;

        // 4. Update .localpaths.toml using the resolver's helper method
        //    We record the *actual* path used for *this specific run*.
        let local_paths_config_path = local_resolver.config_path(); // Get path from resolver

        LocalPathResolver::record_and_save_commit_override(
            local_paths_config_path,
            &self.repo_name,
            &self.commit_hash,
            repo_path, // The absolute path recorded during this specific run
        )
        .context("Failed to record local path override")?;

        println!(
            "Successfully wrote benchmarks for {}/{} at {}",
            self.repo_name, self.subdir_name, self.commit_hash
        );
        Ok(())
    }

    // Optional helper to remove .json files not in the current benchmark list
    #[allow(dead_code)] // Remove if used
    fn cleanup_stale_files(&self, current_benchmarks: &[ErrorAndFixes]) -> Result<()> {
        if !self.suite_path.exists() {
            return Ok(());
        }

        let current_files: std::collections::HashSet<String> = current_benchmarks
            .iter()
            .map(|b| format!("{}.json", b.error_name))
            .collect();

        let entries = fs::read_dir(&self.suite_path).with_context(|| {
            format!(
                "Failed to read suite directory for cleanup: {:?}",
                self.suite_path
            )
        })?;

        for entry_res in entries {
            let entry = entry_res?;
            let path = entry.path();
            if path.is_file() {
                if let Some(filename) = path.file_name().and_then(|n| n.to_str()) {
                    if filename.ends_with(".json") && filename != "git-info.json" && !current_files.contains(filename) {
                        println!("Removing stale benchmark file: {:?}", path);
                        fs::remove_file(&path).with_context(|| {
                            format!("Failed to remove stale file: {:?}", path)
                        })?;
                    }
                }
            }
        }
        Ok(())
    }
}
