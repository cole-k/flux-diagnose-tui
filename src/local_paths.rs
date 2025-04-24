use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::Mutex, // Use Mutex for simple write synchronization
};

// Static mutex to prevent race conditions if multiple generations happen concurrently
// In a real app, consider more robust locking or a dedicated config actor.
static CONFIG_LOCK: Mutex<()> = Mutex::new(());

/// Represents the paths defined for a single repository in .localpaths.toml
#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct RepoLocalPaths {
    /// Key: Full commit hash string
    /// Value: Local path override for that commit
    #[serde(flatten)] // Flattens commit hashes directly into the repo table
    pub commit_paths: HashMap<String, PathBuf>,

    /// Optional: Default path for the repo if no specific commit matches
    #[serde(rename = "_default", skip_serializing_if = "Option::is_none")]
    pub default_path: Option<PathBuf>,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct LocalPathsConfig {
    #[serde(default)] // Use default HashMap if 'repositories' table is missing
    pub repositories: HashMap<String, RepoLocalPaths>,
}

#[derive(Clone)]
pub struct LocalPathResolver {
    config_path: PathBuf,
    config: LocalPathsConfig,
}

impl LocalPathResolver {
    /// Loads the configuration from the specified path.
    /// If the file doesn't exist, returns a resolver with default/empty config.
    pub fn load(config_path: PathBuf) -> Result<Self> {
        let config = if config_path.exists() {
            let content = fs::read_to_string(&config_path)
                .with_context(|| format!("Failed to read local paths config: {:?}", config_path))?;
            toml::from_str(&content)
                .with_context(|| format!("Failed to parse TOML from {:?}", config_path))?
        } else {
            LocalPathsConfig::default()
        };
        Ok(Self {
            config_path,
            config,
        })
    }

    /// Saves the current configuration back to the file.
    /// Acquires a lock to prevent concurrent writes.
    pub fn save(&self) -> Result<()> {
        let _guard = CONFIG_LOCK.lock().unwrap(); // Lock before writing

        let content = toml::to_string_pretty(&self.config)
            .context("Failed to serialize local paths config to TOML")?;

        // Ensure parent directory exists
        if let Some(parent) = self.config_path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create parent directory for config: {:?}", parent)
            })?;
        }

        fs::write(&self.config_path, content).with_context(|| {
            format!(
                "Failed to write local paths config to {:?}",
                self.config_path
            )
        })?;
        Ok(())
    }

    /// Adds or updates a commit-specific local path override.
    pub fn add_commit_override(&mut self, repo_name: &str, commit_hash: &str, local_path: &Path) {
        self.config
            .repositories
            .entry(repo_name.to_string())
            .or_default() // Get or insert the RepoLocalPaths
            .commit_paths
            .insert(commit_hash.to_string(), local_path.to_path_buf());
    }

    /// Adds or updates the default local path override for a repo.
    #[allow(dead_code)]
    pub fn add_default_override(&mut self, repo_name: &str, local_path: &Path) {
        self.config
            .repositories
            .entry(repo_name.to_string())
            .or_default() // Get or insert the RepoLocalPaths
            .default_path = Some(local_path.to_path_buf());
    }

    /// Resolves the local path for a given repo and commit.
    /// Checks commit-specific path first, then the repo's default path.
    /// Returns None if no override is found in the config.
    pub fn resolve(&self, repo_name: &str, commit_hash: &str) -> Option<&PathBuf> {
        self.config
            .repositories
            .get(repo_name)
            .and_then(|repo_paths| {
                // 1. Check specific commit path
                repo_paths
                    .commit_paths
                    .get(commit_hash)
                    // 2. Check default path for the repo
                    .or(repo_paths.default_path.as_ref())
            })
    }

    /// Method used by the *generator* to add the currently used path
    /// as a *commit-specific* override into the config structure.
    /// It then saves the updated config file.
    pub fn record_and_save_commit_override(
        config_path: &Path,
        repo_name: &str,
        commit_hash: &str,
        local_path: &Path,
    ) -> Result<()> {
        // Load the latest config
        let mut resolver = Self::load(config_path.to_path_buf())?;
        // Add the new override
        resolver.add_commit_override(repo_name, commit_hash, local_path);
        // Save it back
        resolver.save()
    }

    pub fn config_path(&self) -> &Path {
        &self.config_path
    }
}
