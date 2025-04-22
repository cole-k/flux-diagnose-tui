use anyhow::Result;
use clap::Parser;
use crossterm::{
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::prelude::*;
use std::{
    collections::HashSet,
    io::stdout,
    path::{Path, PathBuf},
};

mod run_cmd;
mod tui;

use tui::{run_app, AppState, LineLoc};

/// Simple File Viewer with Syntax Highlighting and Fix Input
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Path to the file to view
    file_path: PathBuf,
}

fn main() -> Result<()> {
    let args = Args::parse();

    stdout().execute(EnterAlternateScreen)?;
    enable_raw_mode()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    terminal.clear()?;

    // Example: Mark line 5 as having an error
    let line_loc = LineLoc::new(5, args.file_path.clone());
    let error_lines: HashSet<_> = vec![line_loc].into_iter().collect();
    let error_message = r#"warning: function `centered_rect` is never used
   --> src/main.rs:430:4
    |
430 | fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    |    ^^^^^^^^^^^^^
    |
    = note: `#[warn(dead_code)]` on by default"#
        .to_string();
    let mut app_state = AppState::new(args.file_path.canonicalize()?, error_message, error_lines)?;

    run_app(&mut terminal, &mut app_state)?;

    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;

    // Optional: Print fixes on exit
    if !app_state.fix_lines.is_empty() {
        println!("Fixes proposed:");
        let mut sorted_fixes: Vec<_> = app_state.fix_lines.iter().collect();
        sorted_fixes.sort_by_key(|(k, _)| *k);
        for (line_loc, fix) in sorted_fixes {
            println!("{:?}: {:?}", line_loc, fix);
        }
    }

    let (lines, git_info) =
        run_cmd::run_flux_in_dir(Path::new("/Users/cole/git/flux-examples/kani-vecdeque"))?;

    println!("{:?}", lines);
    println!("Git info: {}", git_info);

    Ok(())
}
