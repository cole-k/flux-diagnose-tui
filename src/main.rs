use anyhow::Result;
use clap::Parser;
use crossterm::{
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::prelude::*;
use std::{
    collections::VecDeque, io::stdout, path::PathBuf
};

mod run_cmd;
mod tui;

use tui::{run_app, AppState, ExitIntent};

/// Simple File Viewer with Syntax Highlighting and Fix Input
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Path to the file to run flux in
    dir: PathBuf,
}

fn main() -> Result<()> {
    let args = Args::parse();

    let (mut lines, git_info) =
        run_cmd::run_flux_in_dir(&args.dir)?;

    let dir_path = git_info.root_path.join(git_info.rel_path_from_root.clone());

    println!("Git info: {}", git_info);

    lines.reverse();
    for line in lines.into_iter() {
        if line.message.level != "error" {
            // println!("skipping level {}", line.message.level);
            continue;
        }
        let rendered_message = line.message.rendered.unwrap();
        let mut error_lines: VecDeque<_> = line.message.spans.iter().flat_map(|span| span.to_line_locs(&dir_path)).collect();
        error_lines.extend(line.message.children.iter().flat_map(|child| {
            // NOTE: this diagnistic is apparently allowed to be recursive, but
            // I sort of doubt it in practice is ever. So I am not recurring.
            child.spans.iter().flat_map(|span| span.to_line_locs(&dir_path))
        }));
        let mut app_state = AppState::new(rendered_message, error_lines)?;
        gather_fix_info(&mut app_state)?;
        if let Some(ExitIntent::Quit) = app_state.exit_intent {
            break;
        }
        if !app_state.fix_lines.is_empty() {
            println!("Fixes proposed:");
            let mut sorted_fixes: Vec<_> = app_state.fix_lines.iter().collect();
            sorted_fixes.sort_by_key(|(k, _)| *k);
            for (line_loc, fix) in sorted_fixes {
                println!("{:?}: {:?}", line_loc, fix);
            }
        }
    }

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
