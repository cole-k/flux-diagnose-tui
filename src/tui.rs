use crate::types::{ErrorAndFixes, Fix, FixLine, LineLoc};
use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Clear, Paragraph, Wrap}, // Added Clear
};
use ratatui_explorer::FileExplorer;
use std::{
    collections::{BTreeMap, VecDeque},
    fs::File,
    io::{BufRead, BufReader, Stdout},
    path::PathBuf,
    time::Duration,
};
use std::{fmt::Write, path::Path};
use syntect::{
    easy::HighlightLines,
    highlighting::{Theme, ThemeSet},
    parsing::SyntaxSet,
};
use tui_input::{backend::crossterm::EventHandler, Input}; // Added tui-input
use unicode_width::UnicodeWidthStr;

// Enum to manage application modes
#[derive(Debug, PartialEq, Eq)]
pub enum AppMode {
    Browsing,
    EditingFix,
    GoToLine,
    AddNote,
    FileExplorer,
    ConfirmationDialog,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfirmationChoice {
    Yes,
    No,
}

struct ConfirmationState {
    title: String,
    body: Option<String>,
    action_on_confirm: Box<dyn FnOnce(&mut AppState, bool) -> Result<()>>,
    current_choice: ConfirmationChoice,
    mode_on_exit: AppMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitIntent {
    /// Quit the entire TUI
    Quit,
    /// Record the fix lines and do this error again
    SaveAndRedo,
    /// Record the fix lines and proceed to next error
    SaveAndNext,
    /// Skip and proceed to next error
    Skip,
}

pub struct AppState {
    error_message: String,
    show_full_error: bool,
    current_file_path: PathBuf,
    /// Saves the location of the (cursor, offset) in each path in case you
    /// return to a file
    file_locations: BTreeMap<PathBuf, (usize, usize)>,
    lines: Vec<String>,
    error_lines: VecDeque<LineLoc>, // 1-indexed line number of lines with errors
    // FIXME: make private when we convert the AppState to some kind of output format.
    pub fix_lines: BTreeMap<LineLoc, Option<String>>, // 1-indexed line number to fix text (None means there is a fix but isn't provided)
    note: Option<String>,
    current_line: usize,  // 0-indexed line number currently selected/focused
    scroll_offset: usize, // 0-indexed line number at the top of the viewport
    pub exit_intent: Option<ExitIntent>,
    syntax_set: SyntaxSet,
    theme: Theme,
    mode: AppMode,               // Current application mode
    input: Input,                // Input field state for tui-input
    editing_line: Option<usize>, // Track which line (1-based) is being edited
    file_explorer: FileExplorer,
    confirmation: Option<ConfirmationState>,
}

impl AppState {
    pub fn new(error_and_fixes: &ErrorAndFixes, dir_path: &Path) -> Result<Self> {
        // Load syntax highlighting defaults
        let syntax_set = SyntaxSet::load_defaults_newlines();
        let theme_set = ThemeSet::load_defaults();
        let theme = theme_set.themes["base16-mocha.dark"].clone();
        let file_explorer_theme = ratatui_explorer::Theme::default().add_default_title();
        let file_explorer = FileExplorer::with_theme(file_explorer_theme)?;

        // let rendered_message = line
        //     .message
        //     .rendered
        //     .unwrap()
        //     .lines()
        //     // Skip the debug information
        //     .filter(|line| !line.contains("constraint_debug_info"))
        //     .collect::<Vec<&str>>()
        //     .join("\n");
        let rendered_message = error_and_fixes.error.message.rendered.clone().unwrap();

        let error_lines: VecDeque<_> = error_and_fixes
            .error_lines
            .iter()
            .map(|line_loc| {
                LineLoc {
                    line: line_loc.line,
                    // Make the lines absolute
                    file: dir_path.join(&line_loc.file).to_path_buf(),
                }
            })
            .collect();

        let current_file_path = error_lines
            .front()
            .context("There are no errors given")?
            .file
            .clone();

        // NOTE: We only will show the first fix: callers must pass
        // an ErrorAndFixes that has 1 or fewer fix in it or risk
        // fixes getting swallowed.
        let (fix_lines, note) = if let Some(fix) = error_and_fixes.fixes.first() {
            let fix_lines = fix.fix_lines
                .iter()
                .map(|fix_line| {
                    let absolute_path = dir_path.join(&fix_line.file);
                    (
                        LineLoc::new(fix_line.line, absolute_path),
                        fix_line.added_reft.clone(),
                    )
                })
                .collect();
            (fix_lines, fix.note.clone())
        } else {
            (BTreeMap::new(), None)
        };

        let mut state = Self {
            error_message: rendered_message,
            show_full_error: true,
            current_file_path,
            file_locations: BTreeMap::new(),
            lines: vec![],
            error_lines: error_lines.clone(),
            fix_lines,
            note,
            current_line: 0,
            scroll_offset: 0,
            exit_intent: None,
            syntax_set,
            theme,
            mode: AppMode::Browsing,
            input: Input::default(),
            editing_line: None,
            file_explorer,
            confirmation: None,
        };

        state.next_error()?;

        Ok(state)
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
        let current_fix = self
            .fix_lines
            .get(&line_loc)
            .cloned()
            .unwrap_or_default()
            .unwrap_or_else(|| "".to_string());
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
        self.current_line = line_no
            .saturating_sub(1)
            .min(self.lines.len().saturating_sub(1));
        self.scroll_offset = self.current_line.saturating_sub(relative_pos);
    }

    fn exit_gotoline_mode(&mut self, go: bool) {
        if go {
            if let Ok(line_no) = self.input.value().parse::<usize>() {
                self.go_to_line(line_no);
            }
        }
        self.input.reset();
        self.mode = AppMode::Browsing;
    }

    fn exit_add_note_mode(&mut self, save_note: bool) {
        if save_note && !self.input.value().is_empty() {
            self.note = Some(self.input.value().to_string());
        } else {
            self.note = None;
        }
        self.initiate_save_and_next();
    }

    fn initiate_save_and_next(&mut self) {
        let (confirmation_title, confirmation_message) = make_confirmation_message(&self.fix_lines);
        self.request_confirmation(
            confirmation_title,
            confirmation_message,
            AppMode::Browsing,
            |app_state, confirmed| {
                if confirmed {
                    app_state.exit_intent = Some(ExitIntent::SaveAndRedo);
                } else {
                    app_state.exit_intent = Some(ExitIntent::SaveAndNext);
                }
                Ok(())
            },
        )
    }

    fn clear_fix(&mut self) {
        let line_to_clear = self.current_line + 1;
        let line_loc = LineLoc::new(line_to_clear, self.current_file_path.clone());
        self.fix_lines.remove(&line_loc);
    }

    fn exit_file_explorer_mode(&mut self, go: bool) -> Result<()> {
        let selected_file = self.file_explorer.current();
        if go && !selected_file.is_dir() {
            self.go_to_file(selected_file.path().clone())?;
        }
        self.mode = AppMode::Browsing;
        Ok(())
    }

    fn go_to_file(&mut self, file: PathBuf) -> Result<()> {
        // Save the current location
        self.file_locations.insert(
            self.current_file_path.clone(),
            (self.current_line, self.scroll_offset),
        );
        // Go to the new location
        self.current_file_path = file;
        let file = File::open(&self.current_file_path)
            .with_context(|| format!("Failed to open file: {:?}", self.current_file_path))?;
        let reader = BufReader::new(file);
        self.lines = reader.lines().collect::<Result<_, _>>()?;
        // Update the location
        (self.current_line, self.scroll_offset) = *self
            .file_locations
            .get(&self.current_file_path)
            .unwrap_or(&(0, 0));
        Ok(())
    }

    fn toggle_confirmation_choice(&mut self) {
        if let Some(state) = self.confirmation.as_mut() {
            let new_choice = match state.current_choice {
                ConfirmationChoice::No => ConfirmationChoice::Yes,
                ConfirmationChoice::Yes => ConfirmationChoice::No,
            };
            state.current_choice = new_choice;
        }
    }

    fn set_confirmation_choice(&mut self, choice: ConfirmationChoice) {
        if let Some(state) = self.confirmation.as_mut() {
            state.current_choice = choice;
        }
    }

    // Helper function to initiate a confirmation request
    fn request_confirmation<F>(
        &mut self,
        title: String,
        body: Option<String>,
        mode_on_exit: AppMode,
        action_on_confirm: F,
    ) where
        // FnOnce: The action runs once.
        // &mut AppState: It can modify the application state.
        // 'static: Necessary because it's stored in the AppState. Usually fine
        //          for closures defined inline that capture state modification logic.
        F: FnOnce(&mut AppState, bool) -> Result<()> + 'static,
    {
        self.confirmation = Some(ConfirmationState {
            title,
            body,
            action_on_confirm: Box::new(action_on_confirm),
            current_choice: ConfirmationChoice::No,
            mode_on_exit,
        });
        self.mode = AppMode::ConfirmationDialog;
    }

    // Helper function to resolve the confirmation
    fn resolve_confirmation(&mut self) -> Result<()> {
        if let Some(state) = self.confirmation.take() {
            (state.action_on_confirm)(self, state.current_choice == ConfirmationChoice::Yes)?;
            self.mode = state.mode_on_exit;
        }
        Ok(())
    }

    // Helper function to cancel the confirmation
    fn _cancel_confirmation(&mut self) {
        if let Some(state) = self.confirmation.take() {
            self.mode = state.mode_on_exit;
        }
    }

    fn next_error(&mut self) -> Result<()> {
        // Should never be empty
        let next_error = self.error_lines.pop_front().unwrap();
        self.go_to_file(next_error.file.clone())?;
        // LineLocs are 1-indexed but so is go to line;
        self.go_to_line(next_error.line);
        self.error_lines.push_back(next_error);
        Ok(())
    }

    pub fn fixes(&self, dir_path: &Path) -> Result<Fix> {
        let fix_lines = self
            .fix_lines
            .iter()
            .map(|(line_loc, fix)| {
                Ok::<FixLine, anyhow::Error>(FixLine {
                    line: line_loc.line,
                    // Make the lines relative to the directory we run in
                    // This is the convention we adopt for saving other files.
                    file: pathdiff::diff_paths(&line_loc.file, dir_path).ok_or_else(|| {
                        anyhow::anyhow!("Couldn't diff path {:?} and {:?}", dir_path, line_loc.file)
                    })?,
                    added_reft: fix.clone(),
                })
            })
            .collect::<Result<_>>()?;
        Ok(Fix {
            fix_lines,
            note: self.note.clone(),
        })
    }
}

pub fn run_app(
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
            if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                app_state.exit_intent = Some(ExitIntent::Quit);
            }
        }

        match app_state.mode {
            AppMode::Browsing => handle_browsing_input(event, app_state, content_height)?,
            AppMode::EditingFix => handle_editing_input(event, app_state)?,
            AppMode::GoToLine => handle_gotoline_input(event, app_state)?,
            AppMode::FileExplorer => handle_file_explorer_input(event, app_state)?,
            AppMode::ConfirmationDialog => handle_confirmation_dialog_input(event, app_state)?,
            AppMode::AddNote => handle_add_note_input(event, app_state)?,
        }

        if app_state.exit_intent.is_some() {
            break;
        }
    }
    Ok(())
}

fn handle_browsing_input(
    event: Event,
    app_state: &mut AppState,
    content_height: usize,
) -> Result<()> {
    if let Event::Key(key) = event {
        if key.kind == KeyEventKind::Press {
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => app_state.request_confirmation(
                    "Really quit?".to_string(),
                    None,
                    AppMode::Browsing,
                    |app_state, confirmed| {
                        if confirmed {
                            app_state.exit_intent = Some(ExitIntent::Quit);
                        }
                        Ok(())
                    },
                ),
                KeyCode::Char('s') => app_state.request_confirmation(
                    "Really skip?".to_string(),
                    None,
                    AppMode::Browsing,
                    |app_state, confirmed| {
                        if confirmed {
                            app_state.exit_intent = Some(ExitIntent::Skip);
                        }
                        Ok(())
                    },
                ),
                KeyCode::Char('z') | KeyCode::Char('n') => {
                    app_state.mode = AppMode::AddNote;
                    // Populate the input field if a note is provided
                    match &app_state.note {
                        Some(note) => {
                            app_state.input = Input::default().with_value(note.clone());
                        }
                        _ => {}
                    }
                }
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
                    app_state.current_line =
                        app_state.current_line.saturating_add(jump).min(max_line);
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
                KeyCode::Char('f') => {
                    app_state.mode = AppMode::FileExplorer;
                }
                KeyCode::Char('h') => {
                    app_state.show_full_error = !app_state.show_full_error;
                }
                KeyCode::Char('e') => {
                    app_state.next_error()?;
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

fn handle_add_note_input(event: Event, app_state: &mut AppState) -> Result<()> {
    if let Event::Key(key) = event {
        match key.code {
            KeyCode::Enter => {
                app_state.exit_add_note_mode(true); // Save on Enter
            }
            KeyCode::Esc => {
                app_state.exit_add_note_mode(false); // Cancel on Esc
            }
            _ => {
                // Pass the event to tui-input
                app_state.input.handle_event(&event);
            }
        }
    }
    Ok(())
}

fn handle_file_explorer_input(event: Event, app_state: &mut AppState) -> Result<()> {
    if let Event::Key(key) = event {
        match key.code {
            // Enter on directories visits them
            KeyCode::Enter if app_state.file_explorer.current().is_dir() => {
                app_state
                    .file_explorer
                    .handle(ratatui_explorer::Input::Right)?;
            }
            // Enter on files exits the explorer
            KeyCode::Enter => {
                app_state.exit_file_explorer_mode(true)?;
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                app_state.exit_file_explorer_mode(false)?;
            }
            _ => {
                app_state.file_explorer.handle(&event)?;
            }
        }
    }
    Ok(())
}

fn handle_confirmation_dialog_input(event: Event, app_state: &mut AppState) -> Result<()> {
    if let Event::Key(key) = event {
        match key.code {
            // Navigation within the dialog
            KeyCode::Left | KeyCode::Right | KeyCode::Tab | KeyCode::BackTab => {
                app_state.toggle_confirmation_choice();
            }
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                // Directly confirm
                app_state.set_confirmation_choice(ConfirmationChoice::Yes);
                app_state.resolve_confirmation()?;
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                // Directly cancel
                app_state.set_confirmation_choice(ConfirmationChoice::No);
                app_state.resolve_confirmation()?;
            }
            KeyCode::Enter => {
                app_state.resolve_confirmation()?;
            }
            _ => {} // Ignore other keys while confirming
        }
    }
    Ok(())
}

fn ui(frame: &mut Frame, app_state: &AppState) {
    let area = frame.area();
    let theme_bg = app_state
        .theme
        .settings
        .background
        .map_or(Color::Reset, |bg| Color::Rgb(bg.r, bg.g, bg.b));

    // --- Main File View ---
    render_file_view(frame, app_state, area, theme_bg);

    // --- Other dialogs ---
    match app_state.mode {
        AppMode::EditingFix => {
            let line_num = app_state.editing_line.unwrap_or(0); // Should always be Some in EditingFix mode
            let title = format!("Enter fix for Line {}:", line_num);
            render_input_dialog(frame, title, 1, app_state, theme_bg);
        }
        AppMode::GoToLine => {
            let title = "Jump to line:".to_string();
            render_input_dialog(frame, title, 1, app_state, theme_bg);
        }
        AppMode::AddNote => {
            let title = "Add any notes:".to_string();
            render_input_dialog(frame, title, 3, app_state, theme_bg);
        }
        AppMode::FileExplorer => {
            render_file_explorer(frame, app_state);
        }
        AppMode::ConfirmationDialog => {
            if let Some(confirm_state) = &app_state.confirmation {
                render_confirmation_dialog(frame, confirm_state);
            }
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

    let visible_lines = app_state
        .lines
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
        let mut line_bg = if is_current_line {
            highlight_bg
        } else {
            theme_bg
        };
        if has_fix {
            line_bg = fix_line_bg;
            // Optionally blend if it's also the current line
            // if is_current_line { line_bg = blend_colors(highlight_bg, fix_line_bg); }
        }

        // 1. Line Number Span
        let line_num_style = Style::default()
            .fg(if is_current_line {
                Color::White
            } else {
                Color::DarkGray
            })
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
                    ratatui_style = ratatui_style
                        .add_modifier(Modifier::UNDERLINED)
                        .underline_color(Color::Red);
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
        let line_num_span = Span::styled(
            format!("{:>width$} ", "~", width = line_number_width as usize),
            line_num_style,
        );
        let padding_span = Span::styled(
            " ".repeat(display_width as usize),
            Style::default().bg(theme_bg),
        );
        text_lines.push(Line::from(vec![line_num_span, padding_span]));
    }

    // --- Create Main Widget ---
    let title = format!(
        " File: {} | Line {}/{} | Offset {} | Mode: {:?} ",
        app_state
            .current_file_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy(),
        app_state.current_line + 1,
        app_state.lines.len(),
        app_state.scroll_offset,
        app_state.mode,
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .title_alignment(Alignment::Center)
        .border_style(Style::default().fg(if app_state.mode == AppMode::Browsing {
            Color::White
        } else {
            Color::DarkGray
        })); // Dim border when editing

    let paragraph = Paragraph::new(text_lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .style(Style::default().bg(theme_bg)); // Ensure paragraph background matches theme

    frame.render_widget(paragraph, area);

    if !app_state.error_message.is_empty() && area.width > 0 && area.height > 0 {
        let error_text_raw = &app_state.error_message;
        let mut popup_content_lines: Vec<String> = Vec::new();
        let popup_width: usize;
        let popup_height: usize;

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
                let y = area.top() + 1; // Place at top-right
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

fn render_input_dialog(
    frame: &mut Frame,
    title: String,
    height: u16,
    app_state: &AppState,
    _theme_bg: Color,
) {
    let input_width = 60; // Desired width of the input box
    let input_height = height + 2; // Height (1 for border, 1 for border)

    let area = frame.area();
    let popup_area = centered_rect_abs(input_width, input_height, area);

    let input_widget = Paragraph::new(app_state.input.value())
        .style(Style::default())
        .wrap(Wrap { trim: false })
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
        (
            popup_area.x + 1 + app_state.input.visual_cursor() as u16,
            // Y position: start of popup + border
            popup_area.y + 1,
        ),
    );
}

fn render_file_explorer(frame: &mut Frame, app_state: &AppState) {
    frame.render_widget(Clear, frame.area());
    frame.render_widget(&app_state.file_explorer.widget(), frame.area());
}

fn render_confirmation_dialog(frame: &mut Frame, confirm_state: &ConfirmationState) {
    let area = frame.area();
    // Define dialog size (e.g., 60% width, fixed height or based on text)
    let popup_width = (area.width as f32 * 0.6).min(60.0) as u16; // Max 60 cells wide
                                                                  // Calculate height needed for message + 1 line for buttons + 2 lines for borders
    let title_lines = textwrap::wrap(&confirm_state.title, popup_width as usize - 2); // -2 for padding/borders
    let body_lines = if let Some(body) = &confirm_state.body {
        textwrap::wrap(body, popup_width as usize - 2)
    } else {
        vec![]
    };
    let popup_height = ((title_lines.len() + body_lines.len()) as u16 + 2 + 2).min(area.height);

    // Center the popup area
    let popup_area = centered_rect_abs(popup_width, popup_height, area);

    // Create the text for Yes/No buttons, highlighting the selected one
    let yes_style = if confirm_state.current_choice == ConfirmationChoice::Yes {
        Style::default().fg(Color::White).bg(Color::Red)
    } else {
        Style::default().fg(Color::LightRed)
    };
    let no_style = if confirm_state.current_choice == ConfirmationChoice::No {
        Style::default().fg(Color::White).bg(Color::Red)
    } else {
        Style::default().fg(Color::LightRed)
    };

    let buttons = Line::from(vec![
        Span::styled(" Yes ", yes_style),
        Span::raw(" | "),
        Span::styled(" No ", no_style),
    ])
    .alignment(Alignment::Center);

    // Combine message, body, and buttons
    let mut text_lines = title_lines
        .iter()
        .map(|s| Line::from(s.as_ref()).alignment(Alignment::Center))
        .collect::<Vec<_>>();
    text_lines.extend(
        body_lines
            .iter()
            .map(|s| Line::from(s.as_ref()).left_aligned())
            .collect::<Vec<_>>(),
    );
    text_lines.push(Line::from("")); // Spacer line
    text_lines.push(buttons);
    let text = Text::from(text_lines);

    // Create the block and paragraph
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));

    let paragraph = Paragraph::new(text)
        .block(block)
        .wrap(Wrap { trim: false }) // Let text wrap inside
        .alignment(Alignment::Center) // Center lines within the block
        .style(Style::default().bg(Color::DarkGray)); // Dialog background

    // Render: Clear area first, then draw the dialog
    frame.render_widget(Clear, popup_area); // Clear the space for the popup
    frame.render_widget(paragraph, popup_area);
}

/// Helper function to create a centered rectangle.
fn _centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
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

/// Generates a summary title and body message confirming proposed fixes. The
/// body is empty if there are no fixes.
///
/// Groups fixes by file and counts total fixes and refinements per file.
///
/// # Arguments
///
/// * `fix_lines` - A map where keys are `LineLoc` (line number and file path)
///                 and values are `Option<String>`, representing an optional
///                 refinement suggestion (`Some`) or just a fix (`None`).
///
fn make_confirmation_message(
    fix_lines: &BTreeMap<LineLoc, Option<String>>,
) -> (String, Option<String>) {
    // 1. Aggregate stats per file
    //    Key: PathBuf (file path)
    //    Value: (total_fixes_in_file, refinement_count_in_file)
    let mut file_stats: BTreeMap<PathBuf, (usize, usize)> = BTreeMap::new();

    for (line_loc, refinement_opt) in fix_lines.iter() {
        let stats = file_stats.entry(line_loc.file.clone()).or_insert((0, 0));
        stats.0 += 1; // Increment total fix count for this file
        if refinement_opt.is_some() {
            stats.1 += 1; // Increment refinement count if Some(String)
        }
    }

    // 2. Calculate totals
    let total_fixes = fix_lines.len();
    let total_files = file_stats.len();

    let title = "Add a different set of fixes for this file?".to_string();

    // 3. Build the output string
    let mut message = format!("Summary: {} fixes in {} files\n", total_fixes, total_files);

    // 4. Sort files alphabetically for consistent output
    let mut sorted_stats: Vec<_> = file_stats.into_iter().collect();
    sorted_stats.sort_by(|(path_a, _), (path_b, _)| path_a.cmp(path_b));
    if sorted_stats.is_empty() {
        return (title, None);
    }

    // 5. Add per-file details
    for (file_path, (fixes, refinements)) in sorted_stats {
        // Use path.display() for a printable representation of the path
        writeln!(
            &mut message,
            "   * {}: {} fixes ({} refinements)",
            file_path.display(),
            fixes,
            refinements
        )
        .unwrap();
    }

    // Remove the trailing newline added by the last writeln! if you don't want it
    // message.pop(); // Optional: depends if you want a trailing newline

    (title, Some(message))
}
