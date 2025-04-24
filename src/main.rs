use anyhow::{anyhow, Context, Result};
use cached_repository::GitWorktreeDir;
// Added Context
use clap::{Args, Parser, Subcommand};
use crossterm::{
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::prelude::*;
use std::{
    collections::HashMap,
    fs,
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
mod evaluator;

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
    #[arg(long)]
    bench_root: PathBuf,
    /// Root directory for caching repositories and worktrees (defaults to .benchmark-cache in parent of bench_root)
    #[arg(long)]
    cache_root: Option<PathBuf>,
}

#[derive(Subcommand, Clone)]
enum Command {
    /// Add benchmarks from DIR to BENCH_DIR by running `flux`
    Add(AddArgs),
    /// Edit benchmarks specified by filters
    Edit(EditArgs),
    /// Evaluate benchmarks specified by filters by running `flux`
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

        let canonical_bench_root = bench_root.canonicalize().unwrap_or_else(|_| bench_root.clone());
        let canonical_cache_root = cache_root.canonicalize().unwrap_or_else(|_| cache_root.clone());

        println!("Using benchmark root: {:?}", canonical_bench_root);
        println!("Using cache root: {:?}", canonical_cache_root);


        match self {
            // Pass cache_root to add/edit/eval run methods
            Self::Add(args) => args.run(local_resolver, bench_root, &cache_root),
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
    /// Use a persistent cached worktree instead of a temporary one (useful for build caching during editing).
    #[arg(long, default_value_t = false)] // Default is temporary for 'add'
    cache: bool,
}

impl AddArgs {
    fn run(&self, local_resolver: LocalPathResolver, bench_root: PathBuf, cache_root: &Path) -> Result<()> {
         // Use absolute path for the input directory for consistency
         let absolute_dir = self.dir.canonicalize().with_context(|| format!("Failed to find or access input directory: {:?}", self.dir))?;
         println!("Processing add command for directory: {:?}", absolute_dir);

        // `add` typically operates on a local directory the user points to.
        // We need Git info *from that directory*.
        // We *don't* use the CachedRepository to check out code here, because
        // the user explicitly provided the source directory.

        let (git_info, repo_path) = run_cmd::discover_git_info(&absolute_dir)?;
        println!("Discovered Git info: {}", git_info);

        // Run flux in the *user-provided* directory.
        // Pass false to disable debug info for add? Maybe not needed.
        let errors_and_fixes = run_cmd::run_flux_in_dir(&absolute_dir, &git_info.commit, false)?;
        if errors_and_fixes.is_empty() {
             println!("No Flux errors found in {}. Nothing to add.", absolute_dir.display());
             return Ok(());
        }
        println!("Found {} potential Flux errors.", errors_and_fixes.len());

        // Create the suite definition based on the discovered git info.
        let mut suite = BenchmarkSuite::new( // Make mutable for write_benchmarks
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
        let existing_benchmarks = suite.load_benchmarks().unwrap_or_default();
        println!("Existing benchmarks found in suite: {}", existing_benchmarks.len());

        // Create a map for quick lookup
        let existing_benchmarks_map: HashMap<_,_> = existing_benchmarks
            .into_iter()
            .map(|b| (b.error_name.clone(), b))
            .collect();


        for error_and_fixes in errors_and_fixes {
             let existing_benchmark_opt = existing_benchmarks_map.get(&error_and_fixes.error_name);

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
                            merged.fixes = existing_benchmark.fixes.clone(); // Keep the old fixes
                            updated_errors_and_fixes.push(merged);
                        }
                    } else {
                        println!("Skipping existing benchmark (use --edit-existing to modify): {}", error_and_fixes.error_name);
                        // If not editing, add the existing one back to ensure it's saved later
                         updated_errors_and_fixes.push(existing_benchmark.clone());
                    }
                }
                None => {
                    // Benchmark doesn't exist, always add it
                    println!("Adding new benchmark: {}", error_and_fixes.error_name);
                    updated_errors_and_fixes.push(error_and_fixes);
                }
            }
        }

        // Ensure benchmarks not found in the current run but existing previously are preserved unless overwritten
         if !self.overwrite_existing && self.edit_existing {
             for (name, existing_benchmark) in existing_benchmarks_map {
                 if !updated_errors_and_fixes.iter().any(|e| e.error_name == name) {
                     println!("Preserving existing benchmark not found in current run: {}", name);
                     updated_errors_and_fixes.push(existing_benchmark);
                 }
             }
         }

        if updated_errors_and_fixes.is_empty() {
             println!("No benchmarks to add or edit after processing. Saving only git-info.");
             // Ensure suite dir exists and write git-info
             suite.write_benchmarks(&[], &git_info)?;
        } else {
            println!("Launching TUI editor for {} benchmarks...", updated_errors_and_fixes.len());
            // Use the discovered absolute repo path here
            // The TUI context should be the directory where flux was run.
            run_tui_editor(
                &absolute_dir,
                &git_info,
                suite, // Pass suite for saving
                updated_errors_and_fixes,
            )?;
        }


        // Update .localpaths.toml using the resolver's helper method
        // Record the *actual* path used for *this specific run*.
        let local_paths_config_path = local_resolver.config_path(); // Get path from resolver

        LocalPathResolver::record_and_save_commit_override(
            local_paths_config_path,
            &git_info.repo_name,
            &git_info.commit,
            &repo_path, // The absolute path recorded during this specific run
        )
        .context("Failed to record local path override")?;

        println!("Add command finished successfully.");
        Ok(())
    }
}


#[derive(Args, Clone)]
struct EditArgs {
    #[command(flatten)]
    benchmarks: BenchmarkArgs,
    /// Use a persistent cached worktree instead of a temporary one (useful for build caching during editing).
    #[arg(long, default_value_t = false)] // Default is temporary for 'edit'
    cache: bool,
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
        let use_cache = self.cache; // Get cache preference from args

        // Define the action closure for editing
        let edit_action = |suite: &BenchmarkSuite,
                           worktree: &GitWorktreeDir| // Updated type
         -> Result<()> {
            println!("Editing suite: {:?}", suite.path());
            println!("Worktree path: {:?}", worktree.path());

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
             // Recreate the suite instance to pass mutable ownership to TUI
             let mut mutable_suite = BenchmarkSuite::new(
                 &bench_root,
                 &git_info.repo_name,
                 &git_info.subdir,
                 &git_info.commit,
             )?;

             // The TUI context should be the relevant subdirectory *within* the worktree
            let tui_context_path = worktree.path().join(&git_info.subdir);
            println!("Editing {} benchmarks in TUI context: {:?}", all_benchmarks_in_suite.len(), tui_context_path);


            run_tui_editor(
                &tui_context_path,
                git_info, // Existing git info from the suite
                mutable_suite, // Pass the mutable suite
                all_benchmarks_in_suite, // The filtered benchmarks to edit
            )?;

            Ok(())
        };

        process_benchmarks(
            &bench_root,
            &self.benchmarks,
            &local_resolver,
            cache_root, // Pass cache_root
            use_cache, // Pass cache preference
            edit_action,
        )
    }
}

#[derive(Args, Clone)]
struct EvalArgs {
    #[command(flatten)]
    benchmarks: BenchmarkArgs,
    /// Use a temporary worktree instead of the default persistent cached one (saves disk space).
    #[arg(long, default_value_t = false)] // Default is cached for 'eval'
    no_cache: bool,
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
        let use_cache = !self.no_cache; // Eval defaults to using cache unless --no-cache is given
        let mut evaluations = vec![];

        // Define the action closure for evaluation
        let eval_action = |suite: &BenchmarkSuite,
                           worktree: &GitWorktreeDir| // Updated type
         -> Result<()> {
            println!(
                "Evaluating suite: {:?} in worktree {:?}",
                suite.path(),
                worktree.path(),
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

            let benchmarks_map: HashMap<String, ErrorAndFixes> = benchmarks_to_run
                                   .into_iter()
                                   .map(|e| (e.error_name.clone(), e))
                                   .collect();

            let git_info = suite.git_info().ok_or_else(|| {
                anyhow!("Cannot run eval for suite {:?}: git-info.json is missing.", suite.path())
            })?;

            // The evaluation context is the relevant subdirectory *within* the worktree
            let eval_subdir = worktree.path().join(&git_info.subdir);


            println!("  Running flux in evaluation subdir: {:?}", eval_subdir);
            // Enable debug info for evaluation to get blame spans
            let flux_errors = run_cmd::run_flux_in_dir(&eval_subdir, &git_info.commit, true)?;
            println!("  Found {} errors from flux run.", flux_errors.len());

            for error in flux_errors {
                //println!("Checking flux error: {}", error.error_name); // Verbose
                if let Some(benchmark_error) = benchmarks_map.get(&error.error_name) {
                    println!("    Evaluating benchmark match: {}", benchmark_error.error_name);
                    match evaluator::extract_constraint_debug_info(&error.error) {
                        Ok(Some(constraint_debug_info)) => {
                            let eval = evaluator::evaluate_error(&constraint_debug_info, benchmark_error);
                            println!("      Evaluation result: {:?}", eval);
                            evaluations.push(eval);
                        },
                        Ok(None) => {
                             eprintln!("      Warning: Could not extract constraint debug info for {}", error.error_name);
                        }
                        Err(e) => {
                             eprintln!("      Warning: Error parsing constraint debug info for {}: {}", error.error_name, e);
                        }
                    }
                } else {
                    //println!("    Flux error {} not found in benchmark suite.", error.error_name); // Verbose
                }
            }


            // 4. Output results as needed (summary printed after loop)
            println!("  Finished evaluation for suite: {:?}", suite.path());

            Ok(())
        };

        process_benchmarks(
            &bench_root,
            &self.benchmarks,
            &local_resolver,
            cache_root, // Pass cache_root
            use_cache,  // Pass cache preference
            eval_action,
        )?;

        println!("\nEvaluation Summary:");
        println!("{}", evaluator::generate_summary_table(&evaluations));

        Ok(())
    }
}

fn main() -> Result<()> {
    // Setup logging (optional, but helpful)
    // Example using env_logger: RUST_LOG=info cargo run ...
    env_logger::init();
    // Consider setting default log level if RUST_LOG not set
    // env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();


    let cli = Cli::parse();
    // Pass cache_root from Cli to command run method
    cli.command.run(cli.bench_root, cli.cache_root)
}

/// Runs the TUI editor on each of the errors_and_fixes, collecting an
/// annotation for them. The save it performs overwrites any existing
/// ErrorAndFix files for the *processed* errors, potentially preserving others.
///
/// # Arguments
/// * `dir_path`: Path where flux command was run / TUI context base (absolute).
/// * `git_info`: Git info (can be from discover or from suite).
/// * `suite`: BenchmarkSuite instance (mutable to allow updating git_info/saving).
/// * `errors_and_fixes`: The benchmarks to process in the TUI.
fn run_tui_editor(
    dir_path: &Path,
    git_info: &GitInformation,
    mut suite: BenchmarkSuite,
    errors_and_fixes_to_process: Vec<ErrorAndFixes>,
) -> Result<()> {
     if errors_and_fixes_to_process.is_empty() {
        println!("TUI: No specific errors provided to process.");
        // If called from 'add' which generated no errors, we might still want to save git_info.
        // If called from 'edit' with filters that matched nothing, we might not.
        // Let's assume if we got here with an empty list, it's likely okay to just return.
        // The 'add' command saves git_info separately *after* this function returns if needed.
        return Ok(());
    }

    println!(
        "Starting TUI for {} error(s) in context: {:?}",
        errors_and_fixes_to_process.len(),
        dir_path
    );

    // Stores the results from the TUI session(s)
    let mut final_benchmarks_map: HashMap<String, ErrorAndFixes> = HashMap::new();

    stdout().execute(EnterAlternateScreen)?;
    enable_raw_mode()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    terminal.clear()?;

    let mut exit_tui = false;
    for initial_error_and_fixes in errors_and_fixes_to_process.into_iter() {
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

        println!("  Processing error: {}", initial_error_and_fixes.error_name); // Log which error we are entering

        for existing_fix_opt in fixes_to_process {
             if exit_tui { break; }

             // Create a temporary ErrorAndFixes with only the current fix (or none)
             // for the AppState initialization. Use the error details from the outer loop.
             let mut current_eaf_for_tui = initial_error_and_fixes.clone();
             current_eaf_for_tui.fixes = existing_fix_opt.cloned().map_or(vec![], |f| vec![f]);

             // println!("    TUI SubLoop: Editing fix (exists: {})", existing_fix_opt.is_some());

            loop { // Inner loop for SaveAndRedo
                 let mut app_state = AppState::new(&current_eaf_for_tui, dir_path)
                     .context("Failed to initialize TUI state")?;

                // Terminal interaction loop
                 if let Err(e) = run_app(&mut terminal, &mut app_state) {
                     // Clean up terminal before propagating error
                     disable_raw_mode()?;
                     stdout().execute(LeaveAlternateScreen)?;
                     return Err(e).context("TUI application error");
                 }

                // Process TUI exit intent
                match app_state.exit_intent {
                     Some(ExitIntent::Quit) | None => { // Treat None as Quit here
                         println!("    TUI intent: Quit");
                         exit_tui = true;
                         break; // Exit inner loop (SaveAndRedo)
                     }
                     Some(ExitIntent::Skip) => {
                         println!("    TUI intent: Skip");
                         // Don't save anything for this fix/error, just break inner loop
                         // Clear any potential fixes added in previous redo loops for this error name
                         // We should perhaps only remove the *specific* fix being edited if skipping?
                         // For simplicity, let's just not add it to final_benchmarks_map
                         break; // Exit inner loop (SaveAndRedo), proceed to next fix/error
                     }
                     Some(ExitIntent::SaveAndNext) | Some(ExitIntent::SaveAndRedo) => {
                         println!("    TUI intent: SaveAndNext or SaveAndRedo");
                         match app_state.fixes(dir_path) {
                             Ok(collected_fix) => {
                                // Check if the fix is actually non-empty before saving
                                if !collected_fix.fix_lines.is_empty() || collected_fix.note.is_some() {
                                     // Aggregate the collected fix into the final map
                                     final_benchmarks_map
                                         .entry(initial_error_and_fixes.error_name.clone())
                                         .or_insert_with(|| {
                                             // Create a new entry if this is the first valid fix for this error
                                             let mut e = initial_error_and_fixes.clone();
                                             e.fixes = Vec::new(); // Start with empty fixes
                                             e
                                         })
                                         .fixes // Modify the fixes vector
                                         .push(collected_fix);
                                     println!("      Collected fix: {} lines, note: {}", final_benchmarks_map.get(&initial_error_and_fixes.error_name).unwrap().fixes.last().unwrap().fix_lines.len(), final_benchmarks_map.get(&initial_error_and_fixes.error_name).unwrap().fixes.last().unwrap().note.is_some());
                                } else {
                                     println!("      Skipping save for empty fix/note.");
                                     // If an empty fix was generated, ensure the error name doesn't persist
                                     // with empty fixes if it was meant to be effectively deleted/skipped.
                                     // Check if any previous fix for this error existed in the map
                                     if let Some(entry) = final_benchmarks_map.get_mut(&initial_error_and_fixes.error_name) {
                                         if entry.fixes.is_empty() {
                                             // If the only fix(es) resulted in empty, remove the whole entry?
                                             // Or just don't add this empty one. Let's not add.
                                         }
                                     }
                                }
                             }
                             Err(e) => {
                                 eprintln!("      Error collecting fixes from TUI state: {}", e);
                                 // If collecting fixes fails, should we retry or skip?
                                 // Let's skip saving this attempt and proceed based on intent.
                                 if matches!(app_state.exit_intent, Some(ExitIntent::SaveAndNext)) {
                                     break; // Skip saving, move to next
                                 } else {
                                     // SaveAndRedo - maybe loop again? Or just break? Let's break.
                                      eprintln!("      Proceeding without saving due to error.");
                                      break; // Exit inner loop
                                 }
                             }
                         };

                        if matches!(app_state.exit_intent, Some(ExitIntent::SaveAndNext)) {
                             println!("    Exiting inner loop (SaveAndNext)");
                             break; // Exit inner loop, proceed to next fix/error
                        } else {
                             println!("    Continuing inner loop (SaveAndRedo)");
                            // SaveAndRedo: loop continues
                        }
                     } // End SaveAnd* match arm
                 } // End match app_state.exit_intent
             } // End inner loop (SaveAndRedo)
         } // End loop over existing fixes
     } // End loop over errors_and_fixes_to_process

    // Restore terminal state regardless of how TUI exited
    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;
    println!("TUI finished.");

    // --- Save the collected/updated benchmarks ---
    // Convert map values back to a Vec
    let final_benchmarks_to_write: Vec<ErrorAndFixes> =
        final_benchmarks_map.into_values().collect();

    if !final_benchmarks_to_write.is_empty() {
        println!(
            "Writing {} benchmarks to suite: {:?}",
            final_benchmarks_to_write.len(),
            suite.path()
        );
         // This overwrites the entire suite's benchmarks with the ones processed/saved in the TUI
         // It also updates git-info.json
         suite.write_benchmarks(
             &final_benchmarks_to_write,
             git_info, // Pass the git_info used for this session
         )?;
    } else if !exit_tui {
         // If TUI wasn't quit, but no benchmarks were saved (e.g., all skipped, or resulted in empty fixes)
         // We should still update git_info if it might have changed (e.g., 'add' context).
         // For 'edit', git_info is likely the same, but writing doesn't hurt.
         // We pass an empty benchmark list, so `write_benchmarks` effectively just updates git_info.
         println!("No benchmarks saved from TUI. Writing potentially updated git-info...");
         suite.write_benchmarks(&[], git_info)?;
     } else {
         println!("TUI was quit. No benchmarks written.");
     }

    Ok(())
}
