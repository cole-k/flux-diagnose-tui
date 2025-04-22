use anyhow::Result;
use clap::Parser;
use crossterm::{
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::prelude::*;
use std::{
    io::stdout,
    path::PathBuf,
};

mod run_cmd;
mod tui;

use tui::{run_app, AppState};

/// Simple File Viewer with Syntax Highlighting and Fix Input
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Path to the file to run flux in
    dir: PathBuf,
}

fn main() -> Result<()> {
    let args = Args::parse();

    let (lines, git_info) =
        run_cmd::run_flux_in_dir(&args.dir)?;

    println!("Git info: {}", git_info);

    stdout().execute(EnterAlternateScreen)?;
    enable_raw_mode()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    terminal.clear()?;

    for line in lines.into_iter().rev() {
        if line.reason != "error" {
            continue;
        }
        let rendered_message = line.message.rendered.unwrap();
        println!("{}", rendered_message);
        // NOTE: message has a `spans` field but I'm not sure it's ever used.
        let error_lines = line.message.children.iter().flat_map(|child| {
            // NOTE: this diagnistic is apparently allowed to be recursive, but
            // I sort of doubt it in practice is ever. So I am not recurring.
            child.spans.iter().flat_map(|span| span.to_line_locs())
        }).collect();
        let mut app_state = AppState::new(rendered_message, error_lines)?;
        run_app(&mut terminal, &mut app_state)?;
        if !app_state.fix_lines.is_empty() {
            println!("Fixes proposed:");
            let mut sorted_fixes: Vec<_> = app_state.fix_lines.iter().collect();
            sorted_fixes.sort_by_key(|(k, _)| *k);
            for (line_loc, fix) in sorted_fixes {
                println!("{:?}: {:?}", line_loc, fix);
            }
        }
    }

    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;

    Ok(())
}
