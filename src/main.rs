use anyhow::{anyhow, Context, Result}; use cached_repository::TempGitWorktreeDir;
// Added Context
use clap::{Args, Parser, Subcommand};
use crossterm::{
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::prelude::*;
use std::{
    collections::HashMap,
    fs,  // For creating cache dir
    io::stdout,
    path::{Path, PathBuf},
};

mod benchmark_processor;
mod benchmark_suite;
mod cached_repository;
mod local_paths;
mod run_cmd;
mod tui;
mod types;

use benchmark_processor::{process_benchmarks, BenchmarkArgs};
use benchmark_suite::BenchmarkSuite;
use local_paths::LocalPathResolver;
use tui::{run_app, AppState, ExitIntent};
use types::{ErrorAndFixes, GitInformation};

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
    /// Directory to read/write benchmarks from
    #[arg(long)] // Ensure it parses as PathBuf
    bench_root: PathBuf,
    /// Root directory for caching repositories (defaults to .benchmark-cache in parent of bench_root)
    #[arg(long)]
    cache_root: Option<PathBuf>,
}

#[derive(Subcommand, Clone)]
enum Command {
    /// Add benchmarks from DIR to BENCH_DIR by running `flux`
    Add(AddArgs),
    /// Edit benchmarks specified by filters
    Edit(EditArgs),
    /// Run `flux` against benchmarks specified by filters
    Eval(EvalArgs),
}

impl Command {
    // Pass cache_root down from Cli
    fn run(&self, bench_root: PathBuf, cache_root_opt: Option<PathBuf>) -> Result<()> {
        let local_paths_config = bench_root.join(".localpaths.toml");
        let local_resolver = LocalPathResolver::load(local_paths_config.clone())?;

        // Determine cache root directory
        let cache_root = cache_root_opt.unwrap_or_else(|| {
            bench_root
                .parent()
                .unwrap_or_else(|| Path::new(".")) // Fallback to current dir if no parent
                .join(".benchmark-cache")
        });

        // Ensure cache root exists
        fs::create_dir_all(&cache_root)
             .with_context(|| format!("Failed to create cache directory: {:?}", cache_root))?;

        println!("Using benchmark root: {:?}", bench_root.canonicalize().unwrap_or_else(|_| bench_root.clone()));
        println!("Using cache root: {:?}", cache_root.canonicalize().unwrap_or_else(|_| cache_root.clone()));


        match self {
            Self::Add(args) => args.run(local_resolver, bench_root),
            // Pass cache_root to edit/eval run methods
            Self::Edit(args) => args.run(local_resolver, bench_root, &cache_root),
            Self::Eval(args) => args.run(local_resolver, bench_root, &cache_root),
        }
    }
}

#[derive(Args, Clone)]
struct AddArgs {
    /// Directory to run `flux` in
    #[arg(value_parser = clap::value_parser!(PathBuf))] // Ensure PathBuf parsing
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
         // Use absolute path for the input directory for consistency
         let absolute_dir = self.dir.canonicalize().with_context(|| format!("Failed to find or access input directory: {:?}", self.dir))?;
         println!("Processing add command for directory: {:?}", absolute_dir);


        let (git_info, repo_path) = run_cmd::discover_git_info(&absolute_dir)?;
        println!("Discovered Git info: {}", git_info);

        let errors_and_fixes = run_cmd::run_flux_in_dir(&absolute_dir, &git_info.commit)?;
        if errors_and_fixes.is_empty() {
             println!("No Flux errors found in {}. Nothing to add.", absolute_dir.display());
             return Ok(());
        }
        println!("Found {} potential Flux errors.", errors_and_fixes.len());

        let suite = BenchmarkSuite::new( // Make mutable for write_benchmarks
            &bench_root,
            &git_info.repo_name,
            &git_info.subdir,
            &git_info.commit,
        )?;
        println!("Target benchmark suite path: {:?}", suite.path());

        if self.overwrite_existing && !self.edit_existing {
            return Err(anyhow!(
                "FATAL: --overwrite-existing passed but --edit-existing not passed"
            ));
        }

        let mut updated_errors_and_fixes = vec![];
        let existing_benchmarks_count = suite.load_benchmarks().unwrap_or_default().len();
        println!("Existing benchmarks found in suite: {}", existing_benchmarks_count);


        for error_and_fixes in errors_and_fixes {
            let existing_benchmark_opt = suite.load_single_benchmark(&error_and_fixes.error_name)?;

            match existing_benchmark_opt {
                Some(existing_benchmark) => {
                    if self.edit_existing {
                        println!("Editing existing benchmark: {}", error_and_fixes.error_name);
                        if self.overwrite_existing {
                             println!("  Overwriting previous fixes for {}.", error_and_fixes.error_name);
                            updated_errors_and_fixes.push(error_and_fixes); // Use the new error/fix data
                        } else {
                            println!("  Merging existing fixes for {}.", error_and_fixes.error_name);
                            // Keep existing fixes, but use the potentially updated error message/spans from the new run
                            let mut merged = error_and_fixes.clone(); // Clone the new error data
                            merged.fixes = existing_benchmark.fixes; // Keep the old fixes
                            updated_errors_and_fixes.push(merged);
                        }
                    } else {
                        println!("Skipping existing benchmark (use --edit-existing to modify): {}", error_and_fixes.error_name);
                        // Skip if not editing existing
                    }
                }
                None => {
                    // Benchmark doesn't exist, always add it
                    println!("Adding new benchmark: {}", error_and_fixes.error_name);
                    updated_errors_and_fixes.push(error_and_fixes);
                }
            }
        }

         println!("Launching TUI editor for {} benchmarks...", updated_errors_and_fixes.len());
         // Use the discovered absolute repo path here
        run_tui_editor(
            &absolute_dir, // The directory where flux was run (for relative paths in TUI)
            &git_info,
            suite,
            updated_errors_and_fixes, // Only pass benchmarks needing TUI interaction
        )?;

        // Update .localpaths.toml using the resolver's helper method
        //    We record the *actual* path used for *this specific run*.
        let local_paths_config_path = local_resolver.config_path(); // Get path from resolver

        LocalPathResolver::record_and_save_commit_override(
            local_paths_config_path,
            &git_info.repo_name,
            &git_info.commit,
            &repo_path, // The absolute path recorded during this specific run
        )
        .context("Failed to record local path override")?;
        Ok(())
    }
}

#[derive(Args, Clone)]
struct EditArgs {
    #[command(flatten)]
    benchmarks: BenchmarkArgs,
}

impl EditArgs {
    // Added cache_root
    fn run(
        &self,
        local_resolver: LocalPathResolver,
        bench_root: PathBuf,
        cache_root: &Path,
    ) -> Result<()> {
        println!("Running Edit command...");

        // Define the action closure for editing
        let edit_action = |suite: &BenchmarkSuite,
                           worktree: &TempGitWorktreeDir|
         -> Result<()> {
            println!("Editing suite: {:?}", suite.path());

            // 1. Load existing benchmarks from the suite
            let mut all_benchmarks_in_suite = suite.load_benchmarks().with_context(|| {
                format!("Failed to load benchmarks from suite: {:?}", suite.path())
            })?;

            // 2. Apply error name filters *within* the suite
            if !self.benchmarks.errors.is_empty() {
                all_benchmarks_in_suite.retain(|b| self.benchmarks.errors.contains(&b.error_name));
                println!("  Applied error filter, {} benchmarks remain.", all_benchmarks_in_suite.len());
            }

            if all_benchmarks_in_suite.is_empty() {
                 println!("  No benchmarks in this suite match the error filters. Skipping TUI.");
                 return Ok(());
            }

            // We need GitInformation to save potentially updated info and for context
            let git_info = suite.git_info().ok_or_else(|| {
                anyhow!(
                    "Cannot edit suite {:?}: git-info.json is missing.",
                    suite.path()
                )
            })?;

             // Make suite mutable for the TUI editor function
             let benchmark_suite = BenchmarkSuite::new(
                 &bench_root,
                 &git_info.repo_name,
                 &git_info.subdir,
                 &git_info.commit,
             )?;

            println!("Editing suite at {:?} (root {:?}) for {} benchmarks", git_info.subdir, worktree.worktree_dir.path(), all_benchmarks_in_suite.len());

            let dir_path = worktree.worktree_dir.path().join(&git_info.subdir);

            run_tui_editor(
                &dir_path,
                git_info, // Existing git info from the suite
                benchmark_suite,
                all_benchmarks_in_suite, // The filtered benchmarks to edit
            )?;

            Ok(())
        };

        process_benchmarks(
            &bench_root,
            &self.benchmarks,
            &local_resolver,
            cache_root, // Pass cache_root
            edit_action,
        )
    }
}

#[derive(Args, Clone)]
struct EvalArgs {
    #[command(flatten)]
    benchmarks: BenchmarkArgs,
    // Add any eval-specific args here, e.g., output format
}

impl EvalArgs {
    // Added cache_root
    fn run(
        &self,
        local_resolver: LocalPathResolver,
        bench_root: PathBuf,
        cache_root: &Path,
    ) -> Result<()> {
        println!("Running Eval command...");

        // Define the action closure for evaluation
        let eval_action = |suite: &BenchmarkSuite,
                           worktree: &TempGitWorktreeDir|
         -> Result<()> {
            println!(
                "Evaluating suite: {:?} in worktree {:?}",
                suite.path(),
                worktree.worktree_dir.path(),
            );

            // 1. Load benchmarks from the suite
            let mut benchmarks_to_run = suite.load_benchmarks().with_context(|| {
                format!("Failed to load benchmarks from suite: {:?}", suite.path())
            })?;

            // 2. Apply error name filters *within* the suite
            if !self.benchmarks.errors.is_empty() {
                benchmarks_to_run.retain(|b| self.benchmarks.errors.contains(&b.error_name));
                 println!("  Applied error filter, {} benchmarks remain.", benchmarks_to_run.len());
            }

             if benchmarks_to_run.is_empty() {
                 println!("  No benchmarks in this suite match the filters. Skipping evaluation.");
                 return Ok(());
            }


            // 3. *** Implement the actual evaluation logic here ***
            //    This likely involves:
            //    - Iterating through `benchmarks_to_run`.
            //    - For each benchmark:
            //        - Constructing the necessary commands (e.g., `cargo flux ...` specific to the benchmark).
            //        - Running the command within the `worktree_path`.
            //            - Use `std::process::Command`.
            //            - Make sure to run it in the correct subdirectory within the worktree,
            //              using the `subdir` from `suite.git_info()`.
            //        - Capturing/parsing the output.
            //        - Comparing results against expected outcomes (if applicable).
            //        - Recording metrics/results.

            let git_info = suite.git_info().ok_or_else(|| {
                anyhow!("Cannot run eval for suite {:?}: git-info.json is missing.", suite.path())
            })?;
            let eval_subdir = worktree.worktree_dir.path().join(&git_info.subdir);


            println!("  Evaluation logic placeholder for subdir: {:?}", eval_subdir);
            for benchmark in benchmarks_to_run {
                println!("    Running evaluation for: {}", benchmark.error_name);
                // Placeholder: Run flux check or specific test
                // let status = Command::new("cargo")
                //     .args(["flux", "--message-format=json"]) // Add specific flags if needed
                //     .current_dir(&eval_subdir)
                //     .status()?;
                // println!("      Command status: {:?}", status);
                 // Add more detailed result processing
                 println!("      (Evaluation not fully implemented yet)");
            }


            // 4. Output results as needed
            println!("  Finished evaluation for suite: {:?}", suite.path());

            Ok(())
        };

        process_benchmarks(
            &bench_root,
            &self.benchmarks,
            &local_resolver,
            cache_root, // Pass cache_root
            eval_action,
        )
    }
}

fn main() -> Result<()> {
    // Setup logging (optional, but helpful)
    // Example using env_logger: RUST_LOG=info cargo run ...
    env_logger::init();

    let cli = Cli::parse();
    // Pass cache_root from Cli to command run method
    cli.command.run(cli.bench_root, cli.cache_root)
}

/// Runs the TUI editor on each of the errors_and_fixes, collecting an
/// annotation for them. The save it performs overwrites any existing
/// ErrorAndFix files.
fn run_tui_editor(
    dir_path: &Path, // Path where flux command was run / TUI context base
    git_info: &GitInformation, // Git info (can be from discover or from suite)
    mut suite: BenchmarkSuite, // Mutable to allow updating git_info inside write_benchmarks
    errors_and_fixes: Vec<ErrorAndFixes>,
) -> Result<()> {
     if errors_and_fixes.is_empty() {
        println!("TUI: No errors to process.");
        // Still save git_info and localpaths if this was triggered by 'add'
        // Check if the suite path exists? No, write_benchmarks creates it.
        // We should probably only call write_benchmarks if there's *something*
        // to write or update (even if it's just git_info).
        // Let's call it unconditionally for now to ensure git_info/localpaths consistency.
         println!("TUI: Writing potentially updated git-info...");
         suite.write_benchmarks(&[], git_info)?;
        return Ok(());
    }

    println!(
        "Starting TUI for {} error(s) in context: {:?}",
        errors_and_fixes.len(),
        dir_path
    );

    // Group fixes by error name after TUI interaction
    let mut final_benchmarks_map: HashMap<String, ErrorAndFixes> = HashMap::new();

    stdout().execute(EnterAlternateScreen)?;
    enable_raw_mode()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    terminal.clear()?;

    let mut exit_tui = false;
    for initial_error_and_fixes in errors_and_fixes.into_iter() {
        if exit_tui {
            break;
        }
        // Process each 'fix' entry within the initial benchmark as a separate TUI session
        // If there are no fixes, create a dummy entry to run the TUI once for annotation.
        let fixes_to_process = if initial_error_and_fixes.fixes.is_empty() {
             vec![None] // Represent "no existing fix" with None
        } else {
             initial_error_and_fixes.fixes.iter().map(Some).collect::<Vec<_>>()
        };

        for existing_fix_opt in fixes_to_process {
             if exit_tui { break; }

             // Create a temporary ErrorAndFixes with only the current fix (or none)
             // for the AppState initialization. Use the error details from the outer loop.
             let mut current_eaf_for_tui = initial_error_and_fixes.clone();
             current_eaf_for_tui.fixes = existing_fix_opt.cloned().map_or(vec![], |f| vec![f]);

             // println!("  TUI SubLoop: Editing fix (exists: {})", existing_fix_opt.is_some());

            loop { // Inner loop for SaveAndRedo
                let mut app_state = AppState::new(&current_eaf_for_tui, dir_path)
                     .context("Failed to initialize TUI state")?;

                if let Err(e) = run_app(&mut terminal, &mut app_state) {
                     // Clean up terminal before propagating error
                     disable_raw_mode()?;
                     stdout().execute(LeaveAlternateScreen)?;
                     return Err(e).context("TUI application error");
                 }

                match app_state.exit_intent {
                    Some(ExitIntent::Quit) | None => {
                        exit_tui = true;
                        break; // Exit inner loop
                    }
                    Some(ExitIntent::Skip) => {
                        break; // Exit inner loop, proceed to next fix/error
                    }
                    Some(ExitIntent::SaveAndNext) | Some(ExitIntent::SaveAndRedo) => {
                        let collected_fix = match app_state.fixes(dir_path) {
                             Ok(f) => f,
                             Err(_) => {
                                 // Decide if we should break or continue based on intent
                                 if matches!(app_state.exit_intent, Some(ExitIntent::SaveAndNext)) {
                                     break; // Exit inner loop if SaveAndNext failed
                                 } else {
                                     continue; // Retry if SaveAndRedo failed
                                 }
                             }
                         };

                        // Aggregate the collected fix into the final map
                        final_benchmarks_map
                            .entry(initial_error_and_fixes.error_name.clone())
                            .and_modify(|e| {
                                // Append the new fix if the error entry already exists
                                e.fixes.push(collected_fix.clone());
                            })
                            .or_insert_with(|| {
                                // Create a new entry if this is the first fix for this error
                                let mut e = initial_error_and_fixes.clone();
                                e.fixes = vec![collected_fix]; // Start with the collected fix
                                e
                            });

                        if matches!(app_state.exit_intent, Some(ExitIntent::SaveAndNext)) {
                            break; // Exit inner loop, proceed to next fix/error
                        } else {
                        }
                    }
                }
            } // End inner loop (SaveAndRedo)
        } // End loop over existing fixes
    } // End loop over errors_and_fixes

    // Restore terminal state regardless of how TUI exited
    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;
    println!("TUI finished.");

    // --- Save the collected/updated benchmarks ---
    let final_benchmarks_to_write: Vec<ErrorAndFixes> =
        final_benchmarks_map.into_values().collect();

    if !final_benchmarks_to_write.is_empty() {
        println!(
            "Writing {} benchmarks to suite: {:?}",
            final_benchmarks_to_write.len(),
            suite.path()
        );
         suite.write_benchmarks(
             &final_benchmarks_to_write,
             git_info,
         )?;
    } else if !exit_tui {
         // If TUI wasn't quit, but no benchmarks were saved (e.g., all skipped),
         // still write git_info/localpath update from the original context.
         println!("No benchmarks saved from TUI. Writing potentially updated git-info and localpath override...");
         suite.write_benchmarks(&[], git_info)?;
     } else {
         println!("TUI was quit. No benchmarks written.");
     }


    Ok(())
}
