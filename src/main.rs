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
use unicode_width::UnicodeWidthStr;
use std::{
    collections::{BTreeMap, HashSet},
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
    GoToLine,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct LineLoc {
    /// 1-indexed
    line: usize,
    file: PathBuf,
}

impl LineLoc {
    fn new(line: usize, file: PathBuf) -> Self {
        Self { line, file }
    }
}

struct AppState {
    error_message: String,
    show_full_error: bool,
    current_file_path: PathBuf,
    lines: Vec<String>,
    error_lines: HashSet<LineLoc>,        // 1-indexed line number of lines with errors
    fix_lines: BTreeMap<LineLoc, Option<String>>, // 1-indexed line number to fix text (None means there is a fix but isn't provided)
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
    fn new(file_path: PathBuf, error_message: String, error_lines: HashSet<LineLoc>) -> Result<Self> {
        let file = File::open(&file_path)
            .with_context(|| format!("Failed to open file: {:?}", file_path))?;
        let reader = BufReader::new(file);
        let lines: Vec<String> = reader.lines().collect::<Result<_, _>>()?;

        // Load syntax highlighting defaults
        let syntax_set = SyntaxSet::load_defaults_newlines();
        let theme_set = ThemeSet::load_defaults();
        let theme = theme_set.themes["base16-ocean.dark"].clone();

        Ok(AppState {
            error_message,
            show_full_error: true,
            current_file_path: file_path,
            lines,
            error_lines,
            fix_lines: BTreeMap::new(),
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
        let line_loc = LineLoc::new(line_to_edit, self.current_file_path.clone());
        self.editing_line = Some(line_to_edit);
        let current_fix = self.fix_lines.get(&line_loc).cloned().unwrap_or_default().unwrap_or_else(||"".to_string());
        self.input = Input::new(current_fix); // Use ::new to set initial value
        self.mode = AppMode::EditingFix;
    }

    fn exit_edit_mode(&mut self, save: bool) {
        if let Some(line_num) = self.editing_line {
            let line_loc = LineLoc::new(line_num, self.current_file_path.clone());
            if save {
                let fix_text = self.input.value().to_string();
                if fix_text.is_empty() {
                    self.fix_lines.insert(line_loc, None);
                } else {
                    self.fix_lines.insert(line_loc, Some(fix_text));
                }
            }
        }
        self.input.reset();
        self.editing_line = None;
        self.mode = AppMode::Browsing;
    }

    // line_no is 1-indexed
    fn go_to_line(&mut self, line_no: usize) {
        let relative_pos = self.current_line.saturating_sub(self.scroll_offset);
        self.current_line = line_no.saturating_sub(1).min(self.lines.len().saturating_sub(1));
        self.scroll_offset = self.current_line.saturating_sub(relative_pos);
    }

    fn exit_gotoline_mode(&mut self, save: bool) {
        if save {
            if let Ok(line_no) = self.input.value().parse::<usize>() {
                self.go_to_line(line_no);
            }
        }
        self.input.reset();
        self.mode = AppMode::Browsing;
    }

    fn clear_fix(&mut self) {
        let line_to_clear = self.current_line + 1;
        let line_loc = LineLoc::new(line_to_clear, self.current_file_path.clone());
        self.fix_lines.remove(&line_loc);
    }
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
    = note: `#[warn(dead_code)]` on by default"#.to_string();
    let mut app_state = AppState::new(args.file_path, error_message, error_lines)?;

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
            AppMode::GoToLine => handle_gotoline_input(event, app_state)?,
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
                KeyCode::Char('g') => {
                    app_state.mode = AppMode::GoToLine;
                }
                KeyCode::Char('h') => {
                    app_state.show_full_error = !app_state.show_full_error;
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

fn handle_gotoline_input(event: Event, app_state: &mut AppState) -> Result<()> {
     if let Event::Key(key) = event {
         match key.code {
             KeyCode::Enter => {
                 app_state.exit_gotoline_mode(true); // Save on Enter
             }
             KeyCode::Esc => {
                 app_state.exit_gotoline_mode(false); // Cancel on Esc
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
    match app_state.mode {
        AppMode::EditingFix => {
            let line_num = app_state.editing_line.unwrap_or(0); // Should always be Some in EditingFix mode
            let title = format!("Enter fix for Line {}:", line_num);
            render_input_dialog(frame, title, app_state, theme_bg);
        }
        AppMode::GoToLine => {
            let title = "Jump to line:".to_string();
            render_input_dialog(frame, title, app_state, theme_bg);
        }
        _ => {}
    }
}

fn render_file_view(frame: &mut Frame, app_state: &AppState, area: Rect, theme_bg: Color) {
    // --- Syntax Highlighting Setup ---
    let syntax = app_state
        .syntax_set
        .find_syntax_by_path(app_state.current_file_path.to_str().unwrap_or(""))
        .or_else(|| {
            app_state.syntax_set.find_syntax_by_extension(
                app_state
                    .current_file_path
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
        let line_loc = LineLoc::new(line_num, app_state.current_file_path.clone());
        let is_current_line = line_idx == app_state.current_line;
        let has_fix = app_state.fix_lines.contains_key(&line_loc);

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
                if app_state.error_lines.contains(&line_loc) {
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
        app_state.current_file_path.file_name().unwrap_or_default().to_string_lossy(),
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

    if !app_state.error_message.is_empty() && area.width > 0 && area.height > 0 {
        let error_text_raw = &app_state.error_message;
        let mut popup_content_lines: Vec<String> = Vec::new();
        let mut popup_width: usize = 0;
        let mut popup_height: usize = 0;

        // Calculate the content, required width, and height based on the mode
        if app_state.show_full_error {
            // Wrap text based on available screen width to determine lines
            let wrapped_lines = textwrap::wrap(error_text_raw, area.width as usize);
            popup_height = wrapped_lines.len().min(area.height as usize); // Cap height

            // Find the widest line among those that will be displayed
            popup_width = wrapped_lines
                .iter()
                .take(popup_height) // Only consider lines that fit vertically
                .map(|line| UnicodeWidthStr::width(line.as_ref()))
                .max()
                .unwrap_or(0)
                .min(area.width as usize); // Cap width

            popup_content_lines = wrapped_lines
                .into_iter()
                .take(popup_height)
                .map(|cow| cow.into_owned())
                .collect();

        } else {
            // Show only the first line, truncated if needed
            if let Some(first_line) = error_text_raw.lines().next() {
                 popup_height = 1;
                 // Calculate width based on visible characters, truncate if needed
                 let max_possible_width = area.width as usize;
                 let mut truncated_line = String::with_capacity(max_possible_width);
                 let mut current_width = 0;
                 for c in first_line.chars() {
                     // Use unicode_width::UnicodeWidthChar::width(c).unwrap_or(1) for char width
                     let char_width = UnicodeWidthStr::width(c.to_string().as_str());
                     if current_width + char_width <= max_possible_width {
                         truncated_line.push(c);
                         current_width += char_width;
                     } else {
                         break; // Stop if adding the next char exceeds width
                     }
                 }
                 popup_width = current_width;
                 popup_content_lines.push(truncated_line);

            } else {
                // Error message exists but is empty or only newlines
                popup_height = 0; // Will prevent rendering
                popup_width = 0;
            }
        }

        // Proceed only if the calculated dimensions are valid
        if popup_width > 0 && popup_height > 0 {
            // Calculate the pop-up area Rect (top-right corner)
            // Add +2 to width/height for borders
            let popup_render_width = (popup_width + 2).min(area.width as usize) as u16;
            let popup_render_height = (popup_height + 2).min(area.height as usize) as u16;

            let popup_rect = {
                let x = area.right().saturating_sub(popup_render_width);
                let y = area.top()+1; // Place at top-right
                Rect::new(x, y, popup_render_width, popup_render_height)
            };

            // Create the pop-up widget (Paragraph wrapped in a Block)
            let popup_text = Text::from(popup_content_lines.join("\n"));

            let popup_paragraph = Paragraph::new(popup_text)
                .style(Style::default().fg(Color::White)) // Text color inside popup
                // No specific alignment needed here, block position handles overall alignment
                .block(
                    Block::default()
                        .title(" Error ")
                        .title_style(Style::default().fg(Color::White).bg(Color::Red).bold())
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::Red))
                        // Set background color for the block area to make it opaque
                        .style(Style::default().bg(Color::DarkGray)), // Background of the popup area
                );

            // Render the popup:
            // 1. Clear the area first (important for transparency if bg wasn't set)
            // 2. Render the popup widget
            frame.render_widget(Clear, popup_rect); // Erase underlying content
            frame.render_widget(popup_paragraph, popup_rect);
        }
    }
}

fn render_input_dialog(frame: &mut Frame, title: String, app_state: &AppState, _theme_bg: Color) {
    let input_width = 60; // Desired width of the input box
    let input_height = 3;  // Height (1 for border, 1 for text, 1 for border)

    let area = frame.area();
    let popup_area = centered_rect_abs(input_width, input_height, area);

    let input_widget = Paragraph::new(app_state.input.value())
        .style(Style::default())
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {} (Enter = Save, Esc = Cancel) ", title))
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
