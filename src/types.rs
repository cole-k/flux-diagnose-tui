use std::{collections::VecDeque, fmt, path::{Path, PathBuf}};

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
    pub added_reft: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fix {
    pub fix_lines: Vec<FixLine>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorAndFixes {
    /// The unique (in this context) name of the error
    pub error_name: String,
    /// The original error from Rust
    pub error: CompilerMessage,
    /// Human-annotated fix information
    pub fixes: Vec<Fix>,
    /// These line locations must be relative to the repo root
    pub error_lines: VecDeque<LineLoc>,
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
            remote_url: convert_ssh_to_https(&remote_url),
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
    pub fn to_line_locs(&self, repo_root: Option<&Path>) -> Vec<LineLoc> {
        let mut output = Vec::with_capacity(1 + self.line_end.saturating_sub(self.line_start));
        // If we don't get a root, we'll keep the path relative.
        let file = repo_root.unwrap_or(Path::new("")).join(Path::new(&self.file_name));
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

/// Converts a Git SSH URL (like git@github.com:user/repo.git or ssh://git@host/path)
/// to an HTTPS URL (https://github.com/user/repo.git).
///
/// If the URL doesn't look like a known SSH format or is already HTTP/HTTPS,
/// it's returned unchanged.
///
/// It prints a warning to stderr when a conversion occurs.
///
/// # Arguments
///
/// * `url_str` - The URL string to potentially convert.
///
/// # Returns
///
/// A `String` containing the HTTPS URL if conversion happened, otherwise the original URL.
fn convert_ssh_to_https(url_str: &str) -> String {
    // Trim whitespace just in case
    let trimmed_url = url_str.trim();

    // 1. Check if it's already http:// or https://
    if trimmed_url.starts_with("https://") || trimmed_url.starts_with("http://") {
        return trimmed_url.to_string();
    }

    // 2. Check for local paths (simple check for common cases)
    // If it looks like a file path, don't try to convert it.
    // This is a basic check and might not cover all edge cases.
     if Path::new(trimmed_url).is_absolute() || trimmed_url.starts_with('.') || trimmed_url.starts_with('/') || trimmed_url.contains('\\') {
        // Heuristic: Looks like a local path, return as is.
         return trimmed_url.to_string();
     }


    // 3. Handle the common SCP-like syntax: [user@]host.xz:path/to/repo.git
    // Example: git@github.com:owner/repo.git
    // Find the first ':' which separates host and path in this syntax.
    // It must exist and not be part of a scheme like `file:///` (already excluded by http check)
    // or immediately after the start like `C:` (partially covered by path check)
    if let Some(colon_pos) = trimmed_url.find(':') {
         // Ensure ':' is not the first character and there's something after it
         if colon_pos > 0 && colon_pos < trimmed_url.len() - 1 {
            // Check that it doesn't contain '/' immediately after the ':', which would indicate a scheme or path
            if &trimmed_url[colon_pos + 1..colon_pos + 2] != "/" {
                // Check if there's an '@' sign before the ':'
                let host_part_start = if let Some(at_pos) = trimmed_url.find('@') {
                    if at_pos < colon_pos {
                        at_pos + 1 // Host starts after '@'
                    } else {
                        0 // '@' is after ':', treat host as starting from the beginning
                    }
                } else {
                    0 // No '@', host starts from the beginning
                };

                let host = &trimmed_url[host_part_start..colon_pos];
                let path = &trimmed_url[(colon_pos + 1)..];

                // Basic validation: host and path should not be empty
                if !host.is_empty() && !path.is_empty() {
                    let new_url = format!("https://{}/{}", host, path);
                    eprintln!(
                        "Warning: Converting potential SSH URL '{}' to HTTPS '{}'. \
                        This is necessary because SSH authentication is not configured. \
                        Cloning may fail if the repository requires authentication or is SSH-only.",
                        trimmed_url, new_url
                    );
                    return new_url;
                }
            }
         }
    }


    // 4. Handle the ssh:// scheme explicitly if present
    // Example: ssh://git@github.com/owner/repo.git
    if let Some(rest) = trimmed_url.strip_prefix("ssh://") {
        if let Some(at_pos) = rest.find('@') {
            // Find the first '/' *after* the '@'
            if let Some(slash_pos) = rest[(at_pos + 1)..].find('/') {
                 let full_slash_pos = at_pos + 1 + slash_pos;
                 // Ensure '@' comes before '/'
                 if at_pos < full_slash_pos {
                    let host_and_path = &rest[(at_pos + 1)..];
                    let new_url = format!("https://{}", host_and_path);
                    eprintln!(
                         "Warning: Converting SSH URL '{}' to HTTPS '{}'. \
                         This is necessary because SSH authentication is not configured. \
                         Cloning may fail if the repository requires authentication or is SSH-only.",
                        trimmed_url, new_url
                    );
                    return new_url;
                }
            }
        }
        // If ssh:// format doesn't match expected user@host/path pattern, maybe return original?
        // Or attempt a simple scheme swap? Let's return original for now if pattern fails.
        // return format!("https://{}", rest); // Alternative: simple scheme swap
    }


    // If none of the patterns matched, return the original URL
    trimmed_url.to_string()
}
