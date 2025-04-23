use anyhow::{bail, Result};
use clap::Parser;
use crossterm::{
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::prelude::*;
use std::{
    collections::{HashMap, VecDeque},
    fs::File,
    io::{stdout, BufRead, BufReader},
    path::{Path, PathBuf},
};
// Use once_cell for lazy compilation of the regex
use once_cell::sync::Lazy;
use regex::Regex;

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

/// Simple File Viewer with Syntax Highlighting and Fix Input
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Path to the file to run flux in
    dir: PathBuf,
}

fn main() -> Result<()> {
    let args = Args::parse();

    let (lines, git_info, repo_path) = run_cmd::run_flux_in_dir(&args.dir)?;

    let dir_path = repo_path.join(git_info.subdir.clone());

    println!("Git info: {}", git_info);

    let mut error_name_numbers: HashMap<String, usize> = HashMap::new();

    let mut error_and_fixes_map: HashMap<String, ErrorAndFixes> = HashMap::new();
    for line in lines.into_iter() {
        if line.message.level != "error" {
            continue;
        }
        // let rendered_message = line
        //     .message
        //     .rendered
        //     .unwrap()
        //     .lines()
        //     // Skip the debug information
        //     .filter(|line| !line.contains("constraint_debug_info"))
        //     .collect::<Vec<&str>>()
        //     .join("\n");
        let rendered_message = line.message.rendered.clone().unwrap();
        let mut error_lines: VecDeque<_> = line
            .message
            .spans
            .iter()
            .flat_map(|span| span.to_line_locs(&dir_path))
            .collect();
        error_lines.extend(line.message.children.iter().flat_map(|child| {
            // NOTE: this diagnistic is apparently allowed to be recursive, but
            // I sort of doubt it in practice is ever. So I am not recurring.
            child
                .spans
                .iter()
                .flat_map(|span| span.to_line_locs(&dir_path))
        }));
        let full_error_name;
        if let Some(first_error) = error_lines.front() {
            let containing_fn_name =
                extract_function_name(&first_error.file, first_error.line)?.unwrap();
            println!("Function name is: {}", containing_fn_name);
            let short_hash = git_info.commit[..7].to_string();
            let error_name = format!(
                "{}-{}-L{}",
                short_hash, containing_fn_name, first_error.line
            );
            let error_name_entry = error_name_numbers
                .entry(error_name.clone())
                // If there is already an error name tracked, update the number
                .and_modify(|n| *n += 1)
                // Otherwise, this is the first
                .or_insert(1);
            full_error_name = format!("{}-{}", error_name, error_name_entry);
            println!("Error name: {}", full_error_name);
        } else {
            println!("No error lines found for the error:\n{}", rendered_message);
            println!("Skipping...");
            continue;
        }
        let mut app_state = AppState::new(rendered_message, error_lines)?;
        gather_fix_info(&mut app_state)?;
        if let Some(ExitIntent::Quit) = app_state.exit_intent {
            break;
        }
        error_and_fixes_map
            .entry(full_error_name)
            .and_modify(|_e| {
                // TODO: Add fix
                //e.fixes.push(fix)
            })
            .or_insert_with_key(|error_name| {
                ErrorAndFixes {
                    error_name: error_name.clone(),
                    error: line.clone(),
                    // TODO: Add fix
                    fixes: vec!(),
                }
            });
        if !app_state.fix_lines.is_empty() {
            println!("Fixes proposed:");
            let mut sorted_fixes: Vec<_> = app_state.fix_lines.iter().collect();
            sorted_fixes.sort_by_key(|(k, _)| *k);
            for (line_loc, fix) in sorted_fixes {
                println!("{:?}: {:?}", line_loc, fix);
            }
        }
    }

    let dir_name = args
        .dir
        .file_name()
        .unwrap_or_else(|| panic!("Cannot get name for path {:?}", args.dir))
        .to_str()
        .expect("Cannot render directory name as string");
    let output_file_name = format!("{}-{}.json", dir_name, git_info.commit);
    println!("Saving output as {}", output_file_name);

    run_benchmark_update(&git_info, &repo_path, &error_and_fixes_map.into_values().collect())?;

    Ok(())
}

fn gather_fix_info(app_state: &mut AppState) -> Result<()> {
    stdout().execute(EnterAlternateScreen)?;
    enable_raw_mode()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    terminal.clear()?;
    run_app(&mut terminal, app_state)?;
    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;
    Ok(())
}

// Compile the regex lazily and globally (or within the function scope if preferred)
// Using Lazy ensures it's compiled only once, safely across threads.
static FUNC_DEF_PATTERN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"^\s*(?:pub(?:\(.*\))?\s+)?(?:async\s+)?(?:unsafe\s+)?fn\s+([a-zA-Z_][a-zA-Z0-9_]*)",
    )
    .expect("Failed to compile function definition regex")
    // Adjusted regex slightly:
    // - Added optional `async` keyword.
    // - Added optional `pub(...)` visibility specifier.
    // - Kept the core fn name capture group `([a-zA-Z_][a-zA-Z0-9_]*)`.
});

// // Define the struct to hold the results, similar to the Python dictionary
// #[derive(Debug, Clone, PartialEq, Eq)]
// pub struct FunctionContext {
//     pub start: usize, // 1-based line number
//     pub end: Option<usize>, // 1-based line number, optional as it might not be found
//     pub name: String,
// }

/// Extracts the function name from the specified file at the given error line.
///
/// Args:
/// * `file_path`: Path to the Rust source file.
/// * `error_line`: The 1-based line number where the error occurred.
/// * `_output_directory`: This parameter was present in Python but unused in the logic.
///                        It's kept here but ignored (marked with `_`). Consider removing if definitely not needed.
///
/// Returns:
/// * `Ok(Some(String))` if the function name is found.
/// * `Ok(None)` if the error line is not inside a recognizable function.
/// * `Err(e)` if there's an error reading the file or other issue.
pub fn extract_function_name(
    file_path: &Path,
    error_line: usize, // Expect 1-based line number
) -> Result<Option<String>> {
    if !file_path.is_file() {
        // Using format! to create a String error message, then boxing it.
        bail!("Cannot find file for context extraction: {:?}", file_path);
    }

    // Read all lines into memory. For very large files, line-by-line processing
    // might be better, but this matches the Python approach.
    let file = File::open(file_path)?;
    let reader = BufReader::new(file);
    let lines: Vec<String> = reader.lines().collect::<Result<_, _>>()?;

    // Ensure error_line is within bounds (adjusting for 0-based index)
    if error_line == 0 || error_line > lines.len() {
        // Or return Ok(None) if an out-of-bounds line just means no context
        bail!(
            "Error line {} is out of bounds for file {:?} ({} lines)",
            error_line,
            file_path,
            lines.len()
        );
    }

    // let mut function_start_line: Option<usize> = None; // 1-based
    let mut function_name: Option<String> = None;

    // Go backwards from the line *before* the error line (0-based index)
    // Saturating sub prevents underflow if error_line is 1.
    for i in (0..=error_line.saturating_sub(1)).rev() {
        let line = &lines[i]; // No need to trim here, regex handles leading space

        if let Some(captures) = FUNC_DEF_PATTERN.captures(line) {
            // Found the function definition line
            // function_start_line = Some(i + 1); // Store 1-based line number
            // Capture group 1 should be the function name
            if let Some(name_match) = captures.get(1) {
                function_name = Some(name_match.as_str().to_string());
            }
            break; // Stop searching backwards
        }
    }

    Ok(function_name)

    // // If we didn't find a function signature before the error line, return None
    // let (Some(fn_start_line), Some(fn_name)) = (function_start_line, function_name) else {
    //     return Ok(None);
    // };

    // // Now find the start of metadata (comments/attributes) preceding the function
    // // Default metadata start is the function start itself
    // let mut metadata_start_line = fn_start_line; // 1-based
    // // Search backwards from the line *before* the function start line (0-based index)
    // for i in (0..fn_start_line.saturating_sub(1)).rev() {
    //     let line = lines[i].trim_start(); // Trim leading whitespace only
    //     // Check for comments, attributes (#), or empty lines
    //     if line.starts_with("//") || line.starts_with('#') || line.is_empty() {
    //          // Continue searching backwards while we find metadata lines
    //          metadata_start_line = i + 1; // Update the potential start (1-based)
    //     } else {
    //         // Found a non-metadata line, the block ended on the *next* line
    //         break;
    //     }
    // }

    // // Find the end of the function using brace counting
    // let mut function_end_line: Option<usize> = None;
    // let mut brace_count: i32 = 0;
    // let mut found_opening_brace = false;

    // // Start searching from the function definition line (0-based index)
    // for i in (fn_start_line - 1)..lines.len() {
    //     let line = &lines[i];
    //     let open_braces = line.matches('{').count() as i32;
    //     let close_braces = line.matches('}').count() as i32;

    //     if !found_opening_brace && open_braces > 0 {
    //         found_opening_brace = true;
    //     }

    //     if found_opening_brace {
    //         brace_count += open_braces;
    //         brace_count -= close_braces;

    //         // If brace_count reaches 0 *after* processing this line,
    //         // and we have found at least one opening brace, this is the end line.
    //         if brace_count <= 0 {
    //              function_end_line = Some(i + 1); // Store 1-based line number
    //              break;
    //         }
    //     } else if i >= fn_start_line && close_braces > 0 {
    //         // Handle edge case: closing brace appears before the first opening brace
    //         // (e.g., malformed code or simple fn like `fn foo();`).
    //         // If we are past the fn definition line and see a '}' before '{',
    //         // assume it's not the function body we are looking for or it's malformed.
    //         // We might stop here or continue, depending on desired robustness.
    //         // For simplicity matching Python, we'll just let the loop continue,
    //         // but `found_opening_brace` prevents premature closing.
    //         // If the function has no body (like `fn foo();`), brace_count will never
    //         // become positive, and function_end_line will remain None.
    //     }
    // }

    // Ok(Some(FunctionContext {
    //     start: metadata_start_line, // Use the found metadata start (1-based)
    //     end: function_end_line,     // 1-based, or None if not found
    //     name: fn_name,
    // }))
}

// This function makes sense for the benchmarks runner to use.
// The benchmark saver should probably just stick to looking in the benchmark suite.
fn run_benchmark_update(git_info: &GitInformation, repo_path: &Path, errors: &Vec<ErrorAndFixes>) -> Result<()> {
    let benchmarks_root = PathBuf::from("./my_benchmarks");
    let cache_root = PathBuf::from("./.benchmark-cache"); // Example cache location
    let local_paths_config = benchmarks_root.parent().unwrap_or(&benchmarks_root).join(".localpaths.toml");

    // --- Setup ---
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
    let mut suite = BenchmarkSuite::new(&benchmarks_root, &git_info.repo_name, &git_info.commit)?;


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
