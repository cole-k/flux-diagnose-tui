use anyhow::{Context, Result};
use clap::Parser;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind}, // Added KeyEvent
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Clear, Paragraph, Wrap}, // Added Clear
};
use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::{stdout, BufRead, BufReader, Stdout},
    path::PathBuf,
    time::Duration,
};
use syntect::{
    easy::HighlightLines,
    highlighting::{Theme, ThemeSet},
    parsing::SyntaxSet,
};
use tui_input::{backend::crossterm::EventHandler, Input}; // Added tui-input

/// Simple File Viewer with Syntax Highlighting and Fix Input
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Path to the file to view
    file_path: PathBuf,
}

// Enum to manage application modes
#[derive(Debug, PartialEq, Eq)]
enum AppMode {
    Browsing,
    EditingFix,
}

struct AppState {
    file_path: PathBuf,
    lines: Vec<String>,
    error_lines: HashSet<usize>,        // 1-indexed line number of lines with errors
    fix_lines: HashMap<usize, String>, // 1-indexed line number to fix text (empty string means cleared/no fix)
    current_line: usize,               // 0-indexed line number currently selected/focused
    scroll_offset: usize,              // 0-indexed line number at the top of the viewport
    should_quit: bool,
    syntax_set: SyntaxSet,
    theme: Theme,
    mode: AppMode,          // Current application mode
    input: Input,           // Input field state for tui-input
    editing_line: Option<usize>, // Track which line (1-based) is being edited
}

impl AppState {
    fn new(file_path: PathBuf, error_lines: HashSet<usize>) -> Result<Self> {
        let file = File::open(&file_path)
            .with_context(|| format!("Failed to open file: {:?}", file_path))?;
        let reader = BufReader::new(file);
        let lines: Vec<String> = reader.lines().collect::<Result<_, _>>()?;

        // Load syntax highlighting defaults
        let syntax_set = SyntaxSet::load_defaults_newlines();
        let theme_set = ThemeSet::load_defaults();
        let theme = theme_set.themes["base16-ocean.dark"].clone();

        Ok(AppState {
            file_path,
            lines,
            error_lines,
            fix_lines: HashMap::new(),
            current_line: 0,
            scroll_offset: 0,
            should_quit: false,
            syntax_set,
            theme,
            mode: AppMode::Browsing,
            input: Input::default(),
            editing_line: None,
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

    fn adjust_scroll(&mut self, viewport_height: usize) {
        let vp_height = viewport_height.max(1);
        if self.current_line < self.scroll_offset {
            self.scroll_offset = self.current_line;
        } else if self.current_line >= self.scroll_offset + vp_height {
            self.scroll_offset = self.current_line.saturating_sub(vp_height - 1);
        }
        let max_scroll_offset = self.lines.len().saturating_sub(vp_height);
        self.scroll_offset = self.scroll_offset.min(max_scroll_offset);
    }

    fn enter_edit_mode(&mut self) {
        let line_to_edit = self.current_line + 1; // Store 1-based index
        self.editing_line = Some(line_to_edit);
        let current_fix = self.fix_lines.get(&line_to_edit).cloned().unwrap_or_default();
        self.input = Input::new(current_fix); // Use ::new to set initial value
        self.mode = AppMode::EditingFix;
    }

    fn exit_edit_mode(&mut self, save: bool) {
        if let Some(line_num) = self.editing_line {
            if save {
                let fix_text = self.input.value().to_string();
                if fix_text.is_empty() {
                    // If empty, treat as cleared, remove the entry
                    self.fix_lines.remove(&line_num);
                } else {
                    self.fix_lines.insert(line_num, fix_text);
                }
            }
        }
        self.input.reset();
        self.editing_line = None;
        self.mode = AppMode::Browsing;
    }

    fn clear_fix(&mut self) {
        let line_to_clear = self.current_line + 1;
        self.fix_lines.remove(&line_to_clear);
    }
}

fn main() -> Result<()> {
    let args = Args::parse();

    stdout().execute(EnterAlternateScreen)?;
    enable_raw_mode()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    terminal.clear()?;

    // Example: Mark line 5 as having an error
    let error_lines: HashSet<usize> = vec![5].into_iter().collect();
    let mut app_state = AppState::new(args.file_path, error_lines)?;

    run_app(&mut terminal, &mut app_state)?;

    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;

    // Optional: Print fixes on exit
    if !app_state.fix_lines.is_empty() {
        println!("Fixes proposed:");
        let mut sorted_fixes: Vec<_> = app_state.fix_lines.iter().collect();
        sorted_fixes.sort_by_key(|(k, _)| *k);
        for (line_num, fix) in sorted_fixes {
            println!("Line {}: {}", line_num, fix);
        }
    }


    Ok(())
}

fn run_app(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app_state: &mut AppState,
) -> Result<()> {
    loop {
        // Adjust scroll based on cursor position before drawing
        let viewport_height = terminal.size()?.height as usize;
        let content_height = viewport_height.saturating_sub(2); // Subtract border heights
        app_state.adjust_scroll(content_height.max(1)); // Ensure content_height > 0

        terminal.draw(|frame| ui(frame, app_state))?;

        if !event::poll(Duration::from_millis(50))? {
            continue;
        }

        let event = event::read()?;

        // Global quit shortcut
        if let Event::Key(key) = event {
             if key.kind == KeyEventKind::Press && (key.code == KeyCode::Char('q') && app_state.mode == AppMode::Browsing || key.code == KeyCode::Esc && app_state.mode == AppMode::Browsing) {
                 app_state.should_quit = true;
             }
        }


        match app_state.mode {
            AppMode::Browsing => handle_browsing_input(event, app_state, content_height)?,
            AppMode::EditingFix => handle_editing_input(event, app_state)?,
        }

        if app_state.should_quit {
            break;
        }
    }
    Ok(())
}

fn handle_browsing_input(event: Event, app_state: &mut AppState, content_height: usize) -> Result<()> {
    if let Event::Key(key) = event {
        if key.kind == KeyEventKind::Press {
            match key.code {
                // Navigation
                KeyCode::Up | KeyCode::Char('k') => app_state.move_up(),
                KeyCode::Down | KeyCode::Char('j') => app_state.move_down(),
                KeyCode::PageUp | KeyCode::Char('u') => {
                    let jump = content_height.saturating_sub(1).max(1);
                    app_state.current_line = app_state.current_line.saturating_sub(jump);
                    app_state.scroll_offset = app_state.scroll_offset.saturating_sub(jump);
                    // Scroll adjustment happens in the main loop
                }
                KeyCode::PageDown | KeyCode::Char('d') => {
                    let jump = content_height.saturating_sub(1).max(1);
                    let max_line = app_state.lines.len().saturating_sub(1);
                    app_state.current_line = app_state.current_line.saturating_add(jump).min(max_line);
                    app_state.scroll_offset = app_state.scroll_offset.saturating_add(jump);
                     // Scroll adjustment happens in the main loop
                }
                KeyCode::Home => app_state.current_line = 0,
                KeyCode::End => {
                    app_state.current_line = app_state.lines.len().saturating_sub(1);
                }

                // Actions
                KeyCode::Enter | KeyCode::Char(' ') => {
                    app_state.enter_edit_mode();
                }
                KeyCode::Char('c') | KeyCode::Char('x') => {
                    app_state.clear_fix();
                }
                _ => {} // Ignore other keys in browsing mode
            }
        }
    }
    Ok(())
}

fn handle_editing_input(event: Event, app_state: &mut AppState) -> Result<()> {
     if let Event::Key(key) = event {
         match key.code {
             KeyCode::Enter => {
                 app_state.exit_edit_mode(true); // Save on Enter
             }
             KeyCode::Esc => {
                 app_state.exit_edit_mode(false); // Cancel on Esc
             }
             _ => {
                 // Pass the event to tui-input
                 app_state.input.handle_event(&event);
             }
         }
     }
     Ok(())
}


fn ui(frame: &mut Frame, app_state: &AppState) {
    let area = frame.area();
    let theme_bg = app_state.theme.settings.background.map_or(Color::Reset, |bg| Color::Rgb(bg.r, bg.g, bg.b));

    // --- Main File View ---
    render_file_view(frame, app_state, area, theme_bg);

    // --- Input Dialog (if active) ---
    if app_state.mode == AppMode::EditingFix {
        render_input_dialog(frame, app_state, theme_bg);
    }
}

fn render_file_view(frame: &mut Frame, app_state: &AppState, area: Rect, theme_bg: Color) {
    // --- Syntax Highlighting Setup ---
    let syntax = app_state
        .syntax_set
        .find_syntax_by_path(app_state.file_path.to_str().unwrap_or(""))
        .or_else(|| {
            app_state.syntax_set.find_syntax_by_extension(
                app_state
                    .file_path
                    .extension()
                    .and_then(|s| s.to_str())
                    .unwrap_or(""),
            )
        })
        .unwrap_or_else(|| app_state.syntax_set.find_syntax_plain_text());

    let mut highlighter = HighlightLines::new(syntax, &app_state.theme);

    // --- Prepare Lines for Display ---
    let mut text_lines: Vec<Line> = Vec::new();
    let display_height = area.height.saturating_sub(2) as usize; // Minus borders
    let line_number_width = app_state.lines.len().to_string().len().max(3) as u16; // Ensure min width
    let display_width = area.width.saturating_sub(3 + line_number_width); // Minus borders & line num space

    let highlight_bg = Color::Rgb(35, 38, 46); // Current line highlight
    let fix_line_bg = Color::Rgb(70, 38, 46); // Background for lines with fixes

    let visible_lines = app_state.lines
        .iter()
        .enumerate()
        .skip(app_state.scroll_offset)
        .take(display_height);

    for (line_idx, line_content) in visible_lines {
        let line_num = line_idx + 1; // 1-based for display and map keys
        let is_current_line = line_idx == app_state.current_line;
        let has_fix = app_state.fix_lines.contains_key(&line_num);

        // Determine line background
        let mut line_bg = if is_current_line { highlight_bg } else { theme_bg };
        if has_fix {
             line_bg = fix_line_bg;
             // Optionally blend if it's also the current line
             // if is_current_line { line_bg = blend_colors(highlight_bg, fix_line_bg); }
        }

        // 1. Line Number Span
        let line_num_style = Style::default()
                                .fg(if is_current_line { Color::White } else { Color::DarkGray })
                                .bg(line_bg); // Use calculated line_bg
        let line_num_span = Span::styled(
            format!("{:>width$} ", line_num, width = line_number_width as usize),
            line_num_style,
        );

        // 2. Content Spans
        let ranges: Vec<(syntect::highlighting::Style, &str)> = highlighter
            .highlight_line(line_content, &app_state.syntax_set)
            .unwrap_or_else(|_| vec![(Default::default(), line_content)]);

        let content_spans: Vec<Span> = ranges
            .into_iter()
            .filter_map(|(syntect_style, content)| {
                let mut ratatui_style = syntect_tui::translate_style(syntect_style).ok()?;
                if app_state.error_lines.contains(&line_num) {
                    ratatui_style = ratatui_style.add_modifier(Modifier::UNDERLINED).underline_color(Color::Red);
                }
                // Apply line_bg to all content spans
                ratatui_style = ratatui_style.bg(line_bg);
                Some(Span::styled(content.to_string(), ratatui_style))
            })
            .collect();

        // 3. Padding Span to fill the rest of the line width
        let content_char_count: usize = content_spans.iter().map(|s| s.width()).sum();
        let padding_needed = display_width.saturating_sub(content_char_count as u16) as usize;
        let padding_span = Span::styled(" ".repeat(padding_needed), Style::default().bg(line_bg));


        // 4. Combine Spans into a Line
        let mut all_spans = vec![line_num_span];
        all_spans.extend(content_spans);
        all_spans.push(padding_span);
        let line = Line::from(all_spans);
        text_lines.push(line);
    }

     // Fill remaining vertical space if content is shorter than display height
     while text_lines.len() < display_height {
         let line_num_style = Style::default().fg(Color::DarkGray).bg(theme_bg);
         let line_num_span = Span::styled(format!("{:>width$} ", "~", width = line_number_width as usize), line_num_style);
         let padding_span = Span::styled(" ".repeat(display_width as usize), Style::default().bg(theme_bg));
         text_lines.push(Line::from(vec![line_num_span, padding_span]));
     }


    // --- Create Main Widget ---
    let title = format!(
        " File: {} | Line {}/{} | Scroll {} | Mode: {:?} ",
        app_state.file_path.file_name().unwrap_or_default().to_string_lossy(),
        app_state.current_line + 1,
        app_state.lines.len(),
        app_state.scroll_offset,
        app_state.mode,
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .title_alignment(Alignment::Center)
        .border_style(Style::default().fg(if app_state.mode == AppMode::Browsing { Color::White } else { Color::DarkGray } )); // Dim border when editing


    let paragraph = Paragraph::new(text_lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .style(Style::default().bg(theme_bg)); // Ensure paragraph background matches theme

    frame.render_widget(paragraph, area);
}

fn render_input_dialog(frame: &mut Frame, app_state: &AppState, _theme_bg: Color) {
    let input_width = 60; // Desired width of the input box
    let input_height = 3;  // Height (1 for border, 1 for text, 1 for border)

    let area = frame.area();
    let popup_area = centered_rect_abs(input_width, input_height, area);

    let line_num = app_state.editing_line.unwrap_or(0); // Should always be Some in EditingFix mode
    let input_widget = Paragraph::new(app_state.input.value())
        .style(Style::default())
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" Enter fix for Line {}: (Enter = Save, Esc = Cancel) ", line_num))
                .border_style(Style::default().fg(Color::Yellow)), // Highlight border
        );

    // Clear the area behind the popup before rendering
    frame.render_widget(Clear, popup_area);
    frame.render_widget(input_widget, popup_area);

    // Set the cursor position within the input box
    // Calculate cursor position relative to the popup area's top-left corner
    frame.set_cursor_position(
        // X position: start of popup + border + visual cursor offset
        (popup_area.x + 1 + app_state.input.visual_cursor() as u16,
        // Y position: start of popup + border
        popup_area.y + 1),
    );
}

/// Helper function to create a centered rectangle.
fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .split(r);

    Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .split(popup_layout[1])[1]
}

/// Helper function to create a centered rectangle with absolute width/height.
fn centered_rect_abs(width: u16, height: u16, r: Rect) -> Rect {
    let h_margin = r.width.saturating_sub(width) / 2;
    let v_margin = r.height.saturating_sub(height) / 2;

    Rect {
        x: r.x + h_margin,
        y: r.y + v_margin,
        width: width.min(r.width),
        height: height.min(r.height),
    }
}
