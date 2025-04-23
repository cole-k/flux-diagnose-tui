use anyhow::{anyhow, Result};
use clap::{Args, Parser, Subcommand};
use crossterm::{
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::prelude::*;
use std::{
    collections::HashMap,
    io::stdout,
    path::{Path, PathBuf},
};

mod run_cmd;
mod tui;
mod local_paths;
mod benchmark_suite;
mod cached_repository;
mod types;

use tui::{run_app, AppState, ExitIntent};
use types::{ErrorAndFixes, GitInformation};
use benchmark_suite::BenchmarkSuite;
use cached_repository::CachedRepository;
use local_paths::LocalPathResolver;

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
    /// Directory to read/write benchmarks from
    #[arg(long)]
    bench_root: PathBuf,
}

#[derive(Subcommand, Clone)]
enum Command {
    /// Add benchmarks from DIR to BENCH_DIR by running `flux`
    Add(AddArgs),
    /// Edit benchmarks in BENCH_DIR
    Edit(EditArgs),
    /// Run `flux` against benchmarks in BENCH_DIR
    Eval(EvalArgs),
}

impl Command {
    fn run(&self, bench_root: PathBuf) -> Result<()> {
        let local_paths_config = bench_root.join(".localpaths.toml");
        let local_resolver = LocalPathResolver::load(local_paths_config.clone())?;
        // let cache_root = cache_root
        //     .unwrap_or(bench_root
        //                .parent()
        //                .ok_or_else(|| anyhow!("Benchmark root {:?} doesn't have a parent", bench_root))?
        //                .join("/.benchmark-cache"));
        match self {
            Self::Add(args)  => args.run(local_resolver, bench_root),
            Self::Edit(args) => args.run(local_resolver, bench_root),
            Self::Eval(args) => args.run(local_resolver, bench_root),
        }
    }
}

#[derive(Args, Clone)]
struct AddArgs {
    /// Directory to run `flux` in
    dir: PathBuf,
    /// Edit any benchmark files that already exist. By default existing
    /// benchmarks are skipped. Note that this updates the error message if flux
    /// generates a new message.
    #[arg(short, long)]
    edit_existing: bool,
    /// Overwrite any benchmark files that already exist (requires
    /// --edit_existing). By default the previous benchmark is loaded.
    #[arg(short = 'f', long)]
    overwrite_existing: bool,
}

impl AddArgs {
    fn run(&self, local_resolver: LocalPathResolver, bench_root: PathBuf) -> Result<()> {
        let (errors_and_fixes, git_info, repo_path) = run_cmd::run_flux_in_dir(&self.dir)?;
        let suite = BenchmarkSuite::new(&bench_root, &git_info.repo_name, &git_info.subdir, &git_info.commit)?;
        if self.overwrite_existing && !self.edit_existing {
            return Err(anyhow!("FATAL: --overwrite-existing passed but --edit-existing not passed"));
        }
        let mut updated_errors_and_fixes = vec!();
        if self.edit_existing {
            for error_and_fixes in errors_and_fixes {
                if let Some(existing_error_and_fix) = suite.load_single_benchmark(&error_and_fixes.error_name)? {
                    if !self.overwrite_existing {
                        // HACK: We'll generate a new ErrorAndFix for every fix. This way the existing fixes
                        // are properly loaded.
                        for fix in existing_error_and_fix.fixes {
                            let mut new_error_and_fix = error_and_fixes.clone();
                            new_error_and_fix.fixes = vec!(fix);
                            updated_errors_and_fixes.push(new_error_and_fix);
                        }
                    } else {
                        updated_errors_and_fixes.push(error_and_fixes);
                    }
                } else {
                    updated_errors_and_fixes.push(error_and_fixes);
                }
            }
        } else {
            for error_and_fixes in errors_and_fixes {
                // Only take the errors that we don't have in our DB already.
                match suite.load_single_benchmark(&error_and_fixes.error_name)? {
                    None => {
                        updated_errors_and_fixes.push(error_and_fixes);
                    }
                    Some(loaded_error_and_fix) if loaded_error_and_fix.fixes.is_empty() => {
                        println!("WARN: {} has no saved fixes but is recorded in the benchmarks, so we're skipping it.", loaded_error_and_fix.error_name);
                    }
                    _ => {}
                }
            }
        }
        run_tui_editor(&self.dir, &repo_path, &git_info, suite, local_resolver, updated_errors_and_fixes)?;
        Ok(())
    }
}

#[derive(Args, Clone)]
struct EditArgs {
    #[command(flatten)]
    benchmarks: BenchmarkArgs,
}

impl EditArgs {
    fn run(&self, local_resolver: LocalPathResolver, bench_root: PathBuf) -> Result<()> {
        Ok(())
    }
}

#[derive(Args, Clone)]
struct EvalArgs {
    #[command(flatten)]
    benchmarks: BenchmarkArgs,
}

impl EvalArgs {
    fn run(&self, local_resolver: LocalPathResolver, bench_root: PathBuf) -> Result<()> {
        Ok(())
    }
}

#[derive(Parser, Clone)]
struct BenchmarkArgs {
    /// A list of strings that select benchmarks matching either: the name of a
    /// repo (exactly), the name of a subdirectory (exactly), or the name of a
    /// commit (7 char or more prefix).
    benchmarks: Vec<String>,
    #[arg(long)]
    cache_root: Option<PathBuf>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    cli.command.run(cli.bench_root)
}

/// Runs the TUI editor on each of the errors_and_fixes, collecting an
/// annotation for them.  The save it performs overwrites any existing
/// ErrorAndFix files.
fn run_tui_editor(dir_path: &Path,
                  repo_path: &Path,
                  // This might be new GitInformation (if we're running flux and importing the errors)
                  // or the same as the one in the suite (if we're running from the suite directly)
                  git_info: &GitInformation,
                  mut suite: BenchmarkSuite,
                  local_resolver: LocalPathResolver,
                  errors_and_fixes: Vec<ErrorAndFixes>)
                  -> Result<()> {
    let mut error_and_fixes_map: HashMap<String, ErrorAndFixes> = HashMap::new();

    stdout().execute(EnterAlternateScreen)?;
    enable_raw_mode()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    terminal.clear()?;

    // For this part we're in raw mode. Printing won't work.
    let mut exit = false;
    for error_and_fixes in errors_and_fixes.into_iter() {
        if exit {
            break;
        }
        loop {
            let mut app_state = AppState::new(&error_and_fixes, &dir_path)?;
            run_app(&mut terminal, &mut app_state)?;
            match app_state.exit_intent {
                // Should never be None, but let's quit if it is just in case.
                Some(ExitIntent::Quit) | None => {
                    exit = true;
                    break;
                }
                Some(ExitIntent::Skip) => {
                    break;
                }
                Some(ExitIntent::SaveAndNext) | Some(ExitIntent::SaveAndRedo) => {
                    let fix = app_state.fixes(&dir_path)?;
                    error_and_fixes_map
                        .entry(error_and_fixes.error_name.clone())
                        .and_modify(|e| {
                            e.fixes.push(fix.clone())
                        })
                        .or_insert_with(|| {
                            let mut e = error_and_fixes.clone();
                            e.fixes = vec!(fix);
                            e
                    });
                    // If the user selects SaveAndRedo, we should keep running.
                    if matches!(app_state.exit_intent, Some(ExitIntent::SaveAndNext)) {
                        break;
                    }
                }
            }
        }
    }

    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;

    suite.write_benchmarks(&error_and_fixes_map.into_values().collect::<Vec<_>>().as_slice(), git_info, &local_resolver, repo_path)?;

    Ok(())
}

// // Define the struct to hold the results, similar to the Python dictionary
// #[derive(Debug, Clone, PartialEq, Eq)]
// pub struct FunctionContext {
//     pub start: usize, // 1-based line number
//     pub end: Option<usize>, // 1-based line number, optional as it might not be found
//     pub name: String,
// }

// This function makes sense for the benchmarks runner to use.
// The benchmark saver should probably just stick to looking in the benchmark suite.
fn run_benchmark_update(git_info: &GitInformation, repo_path: &Path, errors: &Vec<ErrorAndFixes>) -> Result<()> {
    let benchmarks_root = PathBuf::from("./my_benchmarks").canonicalize()?;
    let cache_root = PathBuf::from("./.benchmark-cache").canonicalize()?; // Example cache location
    let local_paths_config = benchmarks_root.join(".localpaths.toml");

    // --- Setup ---
    let local_paths_config = benchmarks_root.join(".localpaths.toml");
    let mut local_resolver = LocalPathResolver::load(local_paths_config.clone())?;
    local_resolver.add_commit_override(&git_info.repo_name, &git_info.commit, repo_path);
    local_resolver.save()?;
    let cache_repo = CachedRepository::new(cache_root, &local_resolver);

    // --- Get Code Directory (Runner's Responsibility) ---
    println!("Getting code directory...");
    let (worktree_path, _guard) = cache_repo.get_worktree(
        &git_info.repo_name,
        &git_info.commit,
        &git_info.remote,
    )?;
    println!("Code available at temporary worktree: {:?}", worktree_path);

    // --- Instantiate Benchmark Suite ---
    // Creates an object representing the suite on disk, tries to load git-info.json
    let mut suite = BenchmarkSuite::new(&benchmarks_root, &git_info.repo_name, &git_info.subdir, &git_info.commit)?;


    // Load existing benchmarks/annotations for this suite
    println!("Loading existing annotations...");
    let _existing_benchmarks = suite.load_benchmarks().unwrap_or_default();

    // --- Write Updated Benchmarks Back ---
    // This saves the *.json files, updates git-info.json, and records the
    // local path override in .localpaths.toml
    println!("Writing updated benchmarks...");
    suite.write_benchmarks(errors, git_info, &local_resolver, repo_path)?;

    // _guard goes out of scope here, automatically cleaning up the worktree
    println!("Worktree guard dropped, cleanup attempted.");

    Ok(())
}
