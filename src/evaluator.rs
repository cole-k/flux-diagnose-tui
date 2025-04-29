use crate::types::{CompilerMessage, Diagnostic, ErrorAndFixes, Fix, FixLine, LineLoc};
use serde::Deserialize;
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use comfy_table::{Table, Row, Cell, ContentArrangement}; // Using comfy-table for easier table generation

// --- Structs for Deserializing ConstraintDebugInfo ---
// These mirror the structure of your Serialize impls, assuming simple JSON types
// for fields with custom serializers.

#[derive(Debug, Clone, Deserialize)]
struct SimpleLocDeserialize {
    line: usize,
    char: usize,
    file: String,
}

#[derive(Debug, Clone, Deserialize)]
struct SimpleSpanDeserialize {
    start: SimpleLocDeserialize,
    end: SimpleLocDeserialize,
}

#[derive(Debug, Clone, Deserialize)]
struct SimpleFnInfoDeserialize {
    fn_name: String,
    fn_span: Option<SimpleSpanDeserialize>,
}

// Assuming BinderOriginator and Name serialize to simple strings for debug output
#[derive(Debug, Clone, Deserialize)]
struct BinderDebugInfoDeserialize {
    name: String, // Read as String based on `serialize_debug`
    pretty_name: Option<String>,
    span: Option<SimpleSpanDeserialize>,
    originator: Option<String>, // Read as String based on `serialize_debug`
    depth: usize,
    related_vars: HashSet<String>, // Read as HashSet<String> based on `serialize_set_debug`
    in_constraint: bool,
    related_function: Option<SimpleFnInfoDeserialize>,
}

#[derive(Debug, Clone, Deserialize)]
struct BlameSpanDebugInfoDeserialize {
    binder_name: String, // Read as String based on `serialize_debug`
    blame_span: Option<SimpleSpanDeserialize>,
    suggested_refinement: Option<String>, // Keep as Option<String>
}

#[derive(Debug, Clone, Deserialize)]
pub struct ConstraintDebugInfoDeserialize { // Made pub as it's returned
    // Assuming rty::Expr serializes to a simple string for debug output
    constraint: String, // Read as String based on `serialize_debug`
    binders: Vec<BinderDebugInfoDeserialize>,
    blame_spans: Vec<BlameSpanDebugInfoDeserialize>,
}

// --- Structs for Evaluation Results ---

#[derive(Debug, Clone)]
pub struct FixEvalResult {
    pub num_correct_lines: usize,
    pub num_correct_lines_all_binders: usize,
    pub missing_lines: Vec<LineLoc>, // Contains the expected lines not found in blame spans
    pub missing_lines_all_binders: Vec<LineLoc>, // Contains the expected lines not found in blame spans
    pub num_total_lines: usize,
    pub is_trivial: Option<bool>,
}

impl FixEvalResult {
    // Helper to calculate the correctness ratio
    pub fn ratio(&self) -> f64 {
        if self.num_total_lines == 0 {
            // If there were no lines to fix, consider it perfectly correct.
            1.0
        } else {
            self.num_correct_lines as f64 / self.num_total_lines as f64
        }
    }

    pub fn is_fully_correct(&self) -> bool {
        self.num_total_lines > 0 && self.num_correct_lines == self.num_total_lines
    }

     pub fn is_partially_correct(&self) -> bool {
        self.num_correct_lines > 0 && self.num_correct_lines < self.num_total_lines
    }

    pub fn is_fully_incorrect(&self) -> bool {
        // Also counts cases with 0 total lines as incorrect for summary purposes,
        // though ratio treats 0/0 as 1.0. Adjust if needed.
        self.num_correct_lines == 0
    }
}

#[derive(Debug, Clone)]
pub struct ErrorEvalResult {
    pub error_name: String,
    pub fix_evals: Vec<FixEvalResult>, // One result per Fix in ErrorAndFixes
}

impl ErrorEvalResult {
     // Finds the best ratio among all evaluated fixes for this error
    pub fn best_ratio(&self) -> f64 {
        self.fix_evals
            .iter()
            .map(|eval| eval.ratio())
            .fold(0.0, f64::max) // Find the maximum ratio
    }

    pub fn is_any_fix_fully_correct(&self) -> bool {
       self.fix_evals.iter().any(|eval| eval.is_fully_correct())
    }

    pub fn is_any_fix_partially_correct(&self) -> bool {
         self.fix_evals.iter().any(|eval| eval.is_partially_correct())
    }

     pub fn is_every_fix_fully_incorrect(&self) -> bool {
        !self.fix_evals.is_empty() && self.fix_evals.iter().all(|eval| eval.is_fully_incorrect())
    }
}

// --- Function Implementations ---

const DEBUG_INFO_PREFIX: &str = "constraint_debug_info: ";

/// Searches for the serialized ConstraintDebugInfo within a CompilerMessage's child notes.
///
/// It checks the `message` field of direct children Diagnostics with level "note".
///
/// # Arguments
/// * `message`: The top-level `CompilerMessage` potentially containing the info.
///
/// # Returns
/// * `Ok(Some(ConstraintDebugInfoDeserialize))` if found and parsed successfully.
/// * `Ok(None)` if no matching note is found.
/// * `Err(anyhow::Error)` if JSON parsing fails.
pub fn extract_constraint_debug_info(
    message: &CompilerMessage,
) -> anyhow::Result<Option<ConstraintDebugInfoDeserialize>> {
    for child_diagnostic in &message.message.children {
        if child_diagnostic.level == "note" {
            if let Some(escaped_json_str) = child_diagnostic.message.strip_prefix(DEBUG_INFO_PREFIX) {
                // let unescaped_json_str = unescaper::unescape(escaped_json_str)?;
                match serde_json::from_str::<ConstraintDebugInfoDeserialize>(&escaped_json_str) {
                    Ok(parsed_info) => {
                        // Successfully parsed
                        return Ok(Some(parsed_info));
                    }
                    Err(e) => {
                        // Parsing failed, return the error
                        return Err(anyhow::anyhow!(
                            "Failed to parse ConstraintDebugInfo JSON: {}. JSON string: '{}'",
                            e, escaped_json_str
                        ));
                    }
                }
            }
        }
        // Potential improvement: Recursively search deeper children?
        // For now, sticking to direct children as per the prompt's implication.
    }

    // No matching note found among direct children
    Ok(None)
}

fn in_fix_line(blame_span: &SimpleSpanDeserialize, fix_line: &FixLine) -> bool {
    // FixLines are relative, just like the blame span paths.
    let fix_line_path_str = fix_line.file.to_string_lossy().to_string();
    // Compare file paths (string comparison - may be fragile)
    blame_span.start.file == fix_line_path_str
    // Is the line contained in the blame span?
        && fix_line.line >= blame_span.start.line
        && fix_line.line <= blame_span.end.line
}

/// Evaluates each `Fix` in `error_and_fixes` against the `blame_spans` in `constraint_info`.
///
/// It checks if the line number of each `FixLine` is contained within any `BlameSpan`'s
/// line range, comparing file paths as strings for now.
///
/// # Arguments
/// * `constraint_info`: The parsed debug information containing blame spans.
/// * `error_and_fixes`: The benchmark data containing expected fixes.
/// * `base_path_for_fixline`: The path relative to which `FixLine.file` should be interpreted.
///                            This is needed to potentially compare against the absolute paths
///                            often found in diagnostic/debug info. If `None`, direct string comparison is used.
///                            *Warning:* Path comparison is complex. This is a basic attempt.
///
/// # Returns
/// * `ErrorEvalResult` summarizing the evaluation for all fixes.
pub fn evaluate_error(
    constraint_info: &ConstraintDebugInfoDeserialize,
    error_and_fixes: &ErrorAndFixes,
) -> ErrorEvalResult {
    let mut fix_evals = Vec::with_capacity(error_and_fixes.fixes.len());

    for fix in &error_and_fixes.fixes {
        let num_total_lines = fix.fix_lines.len();
        let mut num_correct_lines = 0;
        let mut missing_lines = Vec::new();
        let mut num_correct_lines_all_binders = 0;
        let mut missing_lines_all_binders = Vec::new();

        for fix_line in &fix.fix_lines {
            let match_in_blame_spans = constraint_info.blame_spans.iter().any(|blame_span_info| {
                if let Some(blame_span) = &blame_span_info.blame_span {
                    in_fix_line(&blame_span, fix_line)
                } else {
                    false
                }
            });

            let match_in_all_binders = constraint_info.binders.iter().any(|binder_info| {
                if let Some(span) = &binder_info.span {
                    if in_fix_line(span, fix_line) {
                        return true;
                    }
                }
                if let Some(SimpleFnInfoDeserialize {fn_span: Some(span), ..}) = &binder_info.related_function {
                    if in_fix_line(span, fix_line) {
                        return true;
                    }
                }
                false
            });

            if match_in_blame_spans {
                num_correct_lines += 1;
            } else {
                 // Construct LineLoc relative to the base path if provided for reporting
                missing_lines.push(LineLoc::new(fix_line.line, fix_line.file.clone()));
            }

            if match_in_all_binders {
                num_correct_lines_all_binders += 1;
            } else {
                missing_lines_all_binders.push(LineLoc::new(fix_line.line, fix_line.file.clone()));
            }
        }

        fix_evals.push(FixEvalResult {
            num_correct_lines,
            num_correct_lines_all_binders,
            missing_lines,
            missing_lines_all_binders,
            num_total_lines,
            is_trivial: fix.is_trivial,
        });
    }

    ErrorEvalResult {
        error_name: error_and_fixes.error_name.clone(),
        fix_evals,
    }
}


/// Generates a summary table string from a slice of `ErrorEvalResult`s.
///
/// # Arguments
/// * `results`: A slice containing the evaluation results for multiple errors.
///
/// # Returns
/// * A `String` containing the formatted summary table.
pub fn generate_summary_table(results: &[ErrorEvalResult]) -> String {
    if results.is_empty() {
        return "No evaluation results to summarize.".to_string();
    }

    let mut table = Table::new();
    table
        .load_preset(comfy_table::presets::UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec!["Error Name", "Best Ratio", "Fully Correct?", "Fully Incorrect?"]);

    let mut fully_correct_count = 0;
    let mut partially_correct_count = 0;
    let mut fully_incorrect_count = 0;

    for result in results {
         // Find the best performing fix evaluation for this error
        let best_eval_opt = result.fix_evals.iter().max_by(|a, b| {
            a.ratio().partial_cmp(&b.ratio()).unwrap_or(std::cmp::Ordering::Equal)
        });

        let (ratio_str, is_fully_correct, is_fully_incorrect) = match best_eval_opt {
            Some(best_eval) => {
                 let ratio = best_eval.ratio();
                 let correct = best_eval.is_fully_correct();
                 let incorrect = best_eval.is_fully_incorrect(); // Using the stricter definition for summary
                 (
                    format!("{}/{}", best_eval.num_correct_lines, best_eval.num_total_lines),
                    correct,
                    incorrect && !correct, // Ensure mutually exclusive with fully_correct
                 )
            }
            None => {
                 // No fixes were evaluated for this error
                 ("N/A".to_string(), false, true) // Treat as fully incorrect if no fixes
            }
        };

         let fully_correct_marker = if is_fully_correct { "✅" } else { "❌" };
         let fully_incorrect_marker = if is_fully_incorrect { "✅" } else { "❌" };

        table.add_row(vec![
            Cell::new(&result.error_name),
            Cell::new(&ratio_str),
            Cell::new(fully_correct_marker).set_alignment(comfy_table::CellAlignment::Center),
            Cell::new(fully_incorrect_marker).set_alignment(comfy_table::CellAlignment::Center),
        ]);

        // Update summary counts based on the *best* fix for the error
        if is_fully_correct {
            fully_correct_count += 1;
        } else if is_fully_incorrect {
             fully_incorrect_count += 1;
        } else {
             // If not fully correct and not fully incorrect, it must be partial
             // (or N/A without fixes, already counted as incorrect)
             if best_eval_opt.is_some() { // Only count as partial if there were fixes evaluated
                 partially_correct_count += 1;
             }
        }
    }

    // --- Summary Section ---
    let total_errors = results.len();
    let summary = format!(
        "\n--- Summary ---\nTotal Errors Evaluated: {}\nFully Correct (at least one fix): {}\nPartially Correct (best fix): {}\nFully Incorrect (all fixes): {}",
        total_errors, fully_correct_count, partially_correct_count, fully_incorrect_count
    );

    format!("{}\n{}", table, summary)
}
