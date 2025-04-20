use anyhow::{Context, Result};
use clap::Parser;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Paragraph, Wrap},
};
use std::{
    fs::File,
    io::{stdout, BufRead, BufReader, Stdout},
    path::PathBuf,
    time::Duration,
};
use syntect::{
    easy::HighlightLines,
    highlighting::{Theme, ThemeSet},
    parsing::SyntaxSet,
    util::LinesWithEndings,
};
use syntect_tui::into_span; // Use the syntect_tui bridge

/// Simple File Viewer with Syntax Highlighting
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Path to the file to view
    file_path: PathBuf,
}

struct AppState {
    file_path: PathBuf,
    lines: Vec<String>,
    current_line: usize, // 0-indexed line number currently selected/focused
    scroll_offset: usize, // 0-indexed line number at the top of the viewport
    should_quit: bool,
    syntax_set: SyntaxSet,
    theme: Theme,
}

impl AppState {
    fn new(file_path: PathBuf) -> Result<Self> {
        let file = File::open(&file_path)
            .with_context(|| format!("Failed to open file: {:?}", file_path))?;
        let reader = BufReader::new(file);
        let lines: Vec<String> = reader.lines().collect::<Result<_, _>>()?;

        // Load syntax highlighting defaults
        let syntax_set = SyntaxSet::load_defaults_newlines();
        let theme_set = ThemeSet::load_defaults();
        // Using a common theme, you might want to make this configurable
        let theme = theme_set.themes["base16-ocean.dark"].clone();

        Ok(AppState {
            file_path,
            lines,
            current_line: 0,
            scroll_offset: 0,
            should_quit: false,
            syntax_set,
            theme,
        })
    }

    fn move_up(&mut self) {
        if self.current_line > 0 {
            self.current_line -= 1;
        }
    }

    fn move_down(&mut self) {
        if self.current_line < self.lines.len().saturating_sub(1) {
            self.current_line += 1;
        }
    }

    // Adjusts scroll_offset based on current_line and viewport_height
    fn adjust_scroll(&mut self, viewport_height: usize) {
        // Ensure viewport_height is at least 1 to avoid division by zero or underflow
        let vp_height = viewport_height.max(1);

        // Scroll up if current_line is above the viewport
        if self.current_line < self.scroll_offset {
            self.scroll_offset = self.current_line;
        }
        // Scroll down if current_line is below the viewport
        else if self.current_line >= self.scroll_offset + vp_height {
             // Adjust so the current line is the last line in the viewport
            self.scroll_offset = self.current_line - vp_height + 1;
        }

        // Ensure scroll_offset doesn't go beyond what's possible
        let max_scroll_offset = self.lines.len().saturating_sub(vp_height);
        self.scroll_offset = self.scroll_offset.min(max_scroll_offset);
    }
}

fn main() -> Result<()> {
    let args = Args::parse();

    // Initialize terminal
    stdout().execute(EnterAlternateScreen)?;
    enable_raw_mode()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    terminal.clear()?;

    // Create app state
    let mut app_state = AppState::new(args.file_path)?;

    // Run the app loop
    run_app(&mut terminal, &mut app_state)?;

    // Restore terminal
    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;

    Ok(())
}

fn run_app(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app_state: &mut AppState,
) -> Result<()> {
    loop {
        // Adjust scroll based on cursor position before drawing
        let viewport_height = terminal.size()?.height as usize;
        // Subtract border heights if applicable (2 for top/bottom)
        let content_height = viewport_height.saturating_sub(2);
        app_state.adjust_scroll(content_height);


        terminal.draw(|frame| ui(frame, app_state))?;

        // Poll for events with a timeout
        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                // Only handle key presses
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => app_state.should_quit = true,
                        KeyCode::Up | KeyCode::Char('k') => app_state.move_up(),
                        KeyCode::Down | KeyCode::Char('j') => app_state.move_down(),
                        KeyCode::PageUp | KeyCode::Char('u') => {
                            let jump = content_height.saturating_sub(1);
                            app_state.current_line = app_state.current_line.saturating_sub(jump);
                            app_state.scroll_offset = app_state.scroll_offset.saturating_sub(jump);
                        }
                        KeyCode::PageDown | KeyCode::Char('d') => {
                           let jump = content_height.saturating_sub(1);
                            app_state.current_line = app_state.current_line.saturating_add(jump);
                            app_state.scroll_offset = app_state.scroll_offset.saturating_add(jump);
                            // Ensure we don't go past the end
                            app_state.current_line = app_state.current_line.min(app_state.lines.len().saturating_sub(1));
                            // scroll offset is handled in the adjust_scroll hook
                        }
                         KeyCode::Home => {
                            app_state.current_line = 0;
                         }
                         KeyCode::End => {
                            app_state.current_line = app_state.lines.len().saturating_sub(1);
                         }
                        _ => {} // Ignore other keys
                    }
                }
            }
        }

        if app_state.should_quit {
            break;
        }
    }
    Ok(())
}

fn ui(frame: &mut Frame, app_state: &AppState) {
    let area = frame.area();

    // --- Syntax Highlighting Setup ---
    let syntax = app_state
        .syntax_set
        .find_syntax_by_path(app_state.file_path.to_str().unwrap_or(""))
        .or_else(|| app_state.syntax_set.find_syntax_by_extension(
            app_state.file_path.extension().and_then(|s| s.to_str()).unwrap_or(""),
        ))
        .unwrap_or_else(|| app_state.syntax_set.find_syntax_plain_text());

    let mut highlighter = HighlightLines::new(syntax, &app_state.theme);

    // --- Prepare Lines for Display ---
    let mut text_lines: Vec<Line> = Vec::new();
    // Calculate the number of lines that fit in the viewport (minus borders)
    let display_height = area.height.saturating_sub(2) as usize;
    let line_number_width = app_state.lines.len().to_string().len() as u16; // Width for line numbers
    let display_width = area.width.saturating_sub(3 + line_number_width) as usize;

    // Define the highlight color (consider deriving from theme later)
    let highlight_bg = Color::Rgb(35, 38, 46); // A slightly different dark background

    // Extract the theme's background color
    let theme_bg = app_state.theme.settings.background.map_or(
        Color::Reset, // Fallback if theme has no background
        |bg| Color::Rgb(bg.r, bg.g, bg.b),
    );

    for (i, line_content) in app_state.lines[app_state.scroll_offset..app_state.scroll_offset + display_height]
        .iter()
        .enumerate()
    {
        // HACK: pad the line to the viewport width to ensure that it's highlighted. Not sure why we need to do this.
        let padded_line_content = format!("{:<width$}", line_content, width = display_width);
        let line_idx = app_state.scroll_offset + i;
        let is_current_line = line_idx == app_state.current_line;
    
        // 1. Create Line Number Span (Apply highlight BG if needed)
        let line_num = if line_idx >= app_state.scroll_offset + display_height {
            999
        } else {
            line_idx + 1
        };
        let line_num_style = Style::default().fg(Color::DarkGray).bg(theme_bg);
        let line_bg = if is_current_line { highlight_bg } else { theme_bg };
        let line_num_span = Span::styled(
            format!("{:>width$} ", line_num, width = line_number_width as usize),
            line_num_style,
        );
    
        // 2. Create Content Spans (Apply highlight BG if needed)
        let ranges: Vec<(syntect::highlighting::Style, &str)> = highlighter
            .highlight_line(&padded_line_content, &app_state.syntax_set)
            .unwrap_or_else(|_| vec![(Default::default(), &padded_line_content)]);
    
        let content_spans: Vec<Span> = ranges
            .into_iter()
            .filter_map(|(syntect_style, content)| {
                // Convert syntect style to ratatui style
                let ratatui_style = syntect_tui::translate_style(syntect_style).ok()?;

                Some(Span::styled(content.to_string(), ratatui_style.bg(line_bg)))
            })
            .collect();
    
        // 3. Combine Spans into a Line
        let mut all_spans = vec![line_num_span];
        all_spans.extend(content_spans);
        let line = Line::from(all_spans);
    
        text_lines.push(line);
    }
    
    // --- Create Widgets ---
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(
            " File: {} (Line {}/{}) (Scroll offset {})",
            app_state.file_path.file_name().unwrap_or_default().to_string_lossy(),
            app_state.current_line + 1, // Display 1-based line number
            app_state.lines.len(),
            app_state.scroll_offset,
        ))
        .title_alignment(Alignment::Center);

    let paragraph = Paragraph::new(text_lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .style(Style::default().bg(theme_bg));

    // --- Render ---
    frame.render_widget(paragraph, area);
}
