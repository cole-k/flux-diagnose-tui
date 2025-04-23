use std::{fmt, path::{Path, PathBuf}};

use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct LineLoc {
    /// 1-indexed
    pub line: usize,
    pub file: PathBuf,
}

impl LineLoc {
    pub fn new(line: usize, file: PathBuf) -> Self {
        Self { line, file }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixLine {
    pub line: usize,
    /// This should be relative to the error run_dir
    pub file: PathBuf,
    pub fix: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fix {
    pub fix_lines: Vec<FixLine>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorAndFixes {
    pub error_name: String,
    pub error: CompilerMessage,
    pub fixes: Vec<Fix>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteInfo {
    pub remote_name: String,
    pub remote_url: String,
}

impl RemoteInfo {
    pub fn new(remote_name: String, remote_url: String) -> Self {
        Self {
            remote_name,
            remote_url,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitInformation {
    pub repo_name: String,
    pub commit: String,
    pub remote: Option<RemoteInfo>,
    pub branch: String,
    /// The path to the subdirectory to run in (could just be the root).
    pub subdir: PathBuf,
}

impl fmt::Display for GitInformation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Commit: {}, Branch: {}, Remote: {}, Relative Path: {}",
            &self.commit[..7],
            self.branch,
            self.remote.clone().map(|remote| format!("{} (URL: {})", remote.remote_name, remote.remote_url)).unwrap_or("<none>".to_string()),
            self.subdir.display()
        )
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CompilerMessage {
    pub reason: String, // e.g., "compiler-message"
    pub package_id: String,
    pub manifest_path: PathBuf, // Using PathBuf for file paths
    pub target: Target,
    pub message: Diagnostic,
}

// --- Target Structure ---

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Target {
    pub kind: Vec<String>,        // e.g., ["dylib", "rlib"]
    pub crate_types: Vec<String>, // e.g., ["dylib", "rlib"]
    pub name: String,
    pub src_path: PathBuf, // Using PathBuf for file paths
    pub edition: String,   // e.g., "2021"
    pub doc: bool,
    pub doctest: bool,
    pub test: bool,
}

// --- Diagnostic Structure (Recursive) ---

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Diagnostic {
    // The main message string. Often present at the top level.
    pub message: String,

    // The error/warning code, e.g., "E0999". Optional.
    pub code: Option<DiagnosticCode>,

    // Severity level, e.g., "error", "warning", "note"
    pub level: String, // Could be an Enum later if needed

    // Associated source code locations
    pub spans: Vec<RustSpan>,

    // Nested diagnostic messages (often notes or help messages)
    pub children: Vec<Diagnostic>,

    // The fully rendered message string. Often null in children.
    pub rendered: Option<String>,

    // This field seems specific to the top-level message object,
    // using rename for the '$' and Option since it might not be in children.
    #[serde(rename = "$message_type")]
    pub message_type: Option<String>, // e.g., "diagnostic"
}

// --- Diagnostic Code Structure ---

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DiagnosticCode {
    pub code: String,                // e.g., "E0999"
    pub explanation: Option<String>, // Often null
}

// --- Span Structure ---

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RustSpan {
    pub file_name: String,
    pub byte_start: usize,
    pub byte_end: usize,
    pub line_start: usize,
    pub line_end: usize,
    pub column_start: usize,
    pub column_end: usize,
    pub is_primary: bool,         // Is this the primary span for the diagnostic?
    pub text: Vec<TextHighlight>, // The code snippet associated with the span
    pub label: Option<String>, // Label displayed with the span, e.g., "a precondition cannot be proved"
    pub suggested_replacement: Option<String>, // Code suggestion
    pub suggestion_applicability: Option<String>, // e.g., "MachineApplicable", "HasPlaceholders", etc. (Could be Enum)
}

impl RustSpan {
    pub fn to_line_locs(&self, repo_root: &Path) -> Vec<LineLoc> {
        let mut output = Vec::with_capacity(1 + self.line_end.saturating_sub(self.line_start));
        let file = repo_root.join(Path::new(&self.file_name));
        let mut line = self.line_start;
        while line <= self.line_end {
            output.push(LineLoc::new(line, file.clone()));
            line += 1;
        }
        output
    }
}

// --- Text Highlight Structure (within Span) ---

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TextHighlight {
    pub text: String,           // The line of code
    pub highlight_start: usize, // 1-based column index
    pub highlight_end: usize,   // 1-based column index
}
