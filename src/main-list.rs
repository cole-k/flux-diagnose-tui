// use anyhow::{Context, Result};
// use clap::Parser;
// use crossterm::{
//     event::{self, Event, KeyCode, KeyEventKind},
//     terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
//     ExecutableCommand,
// };
// use ratatui::{
//     prelude::*,
//     // Import List, ListItem, ListState
//     widgets::{Block, Borders, List, ListItem, ListState, Wrap},
// };
// use std::{
//     fs::File,
//     io::{stdout, BufRead, BufReader, Stdout},
//     path::PathBuf,
//     time::Duration,
// };
// use syntect::{
//     easy::HighlightLines,
//     highlighting::{Theme, ThemeSet},
//     parsing::SyntaxSet,
//     // Removed unused import: LinesWithEndings
// };
// // Removed unused import: syntect_tui::into_span (translate_style is used directly)
// use syntect_tui; // Use the syntect_tui bridge

// /// Simple File Viewer with Syntax Highlighting
// #[derive(Parser, Debug)]
// #[command(version, about, long_about = None)]
// struct Args {
//     /// Path to the file to view
//     file_path: PathBuf,
// }

// struct AppState {
//     file_path: PathBuf,
//     lines: Vec<String>,
//     // current_line is now implicitly managed by list_state.selected()
//     // We keep it for convenience in adjust_scroll and title, but ensure it's synced.
//     current_line_index: usize,
//     scroll_offset: usize, // 0-indexed line number at the top of the viewport
//     list_state: ListState, // Use ListState for selection
//     should_quit: bool,
//     syntax_set: SyntaxSet,
//     theme: Theme,
// }

// impl AppState {
//     fn new(file_path: PathBuf) -> Result<Self> {
//         let file = File::open(&file_path)
//             .with_context(|| format!("Failed to open file: {:?}", file_path))?;
//         let reader = BufReader::new(file);
//         let lines: Vec<String> = reader.lines().collect::<Result<_, _>>()?;

//         // Load syntax highlighting defaults
//         let syntax_set = SyntaxSet::load_defaults_newlines();
//         let theme_set = ThemeSet::load_defaults();
//         let theme = theme_set.themes["base16-ocean.dark"].clone();

//         // Initialize ListState
//         let mut list_state = ListState::default();
//         let initial_selection = 0;
//         if !lines.is_empty() {
//             list_state.select(Some(initial_selection));
//         }


//         Ok(AppState {
//             file_path,
//             lines,
//             current_line_index: initial_selection,
//             scroll_offset: 0,
//             list_state, // Store the state
//             should_quit: false,
//             syntax_set,
//             theme,
//         })
//     }

//     // Helper to sync current_line_index after list_state changes
//     fn sync_current_line(&mut self) {
//         self.current_line_index = self.list_state.selected().unwrap_or(0);
//     }

//     // --- Movement methods now update ListState ---

//     fn move_up(&mut self) {
//         if let Some(selected) = self.list_state.selected() {
//             if selected > 0 {
//                 self.list_state.select(Some(selected - 1));
//             }
//         } else if !self.lines.is_empty() {
//              // Select the first line if nothing is selected
//              self.list_state.select(Some(0));
//         }
//         self.sync_current_line();
//     }

//     fn move_down(&mut self) {
//         if let Some(selected) = self.list_state.selected() {
//             let max_index = self.lines.len().saturating_sub(1);
//             if selected < max_index {
//                 self.list_state.select(Some(selected + 1));
//             }
//         } else if !self.lines.is_empty() {
//              // Select the first line if nothing is selected
//              self.list_state.select(Some(0));
//         }
//         self.sync_current_line();
//     }

//      fn page_up(&mut self, viewport_height: usize) {
//         if let Some(selected) = self.list_state.selected() {
//             let jump = viewport_height.saturating_sub(1).max(1);
//             let new_selection = selected.saturating_sub(jump);
//             self.list_state.select(Some(new_selection));
//         } else if !self.lines.is_empty() {
//              self.list_state.select(Some(0));
//         }
//         self.sync_current_line();
//     }

//     fn page_down(&mut self, viewport_height: usize) {
//         if let Some(selected) = self.list_state.selected() {
//              let jump = viewport_height.saturating_sub(1).max(1);
//              let max_index = self.lines.len().saturating_sub(1);
//              let new_selection = selected.saturating_add(jump).min(max_index);
//              self.list_state.select(Some(new_selection));
//         } else if !self.lines.is_empty() {
//              self.list_state.select(Some(0));
//         }
//         self.sync_current_line();
//     }

//     fn go_home(&mut self) {
//         if !self.lines.is_empty() {
//             self.list_state.select(Some(0));
//             self.sync_current_line();
//         }
//     }

//     fn go_end(&mut self) {
//         if !self.lines.is_empty() {
//             let last_index = self.lines.len() - 1;
//             self.list_state.select(Some(last_index));
//             self.sync_current_line();
//         }
//     }


//     // Adjusts scroll_offset based on current_line_index and viewport_height
//     // This logic remains largely the same, but uses the synced current_line_index
//     fn adjust_scroll(&mut self, viewport_height: usize) {
//         let vp_height = viewport_height.max(1);
//         let current_selection = self.current_line_index; // Use the synced index

//         // Scroll up if current_line is above the viewport
//         if current_selection < self.scroll_offset {
//             self.scroll_offset = current_selection;
//         }
//         // Scroll down if current_line is below the viewport
//         else if current_selection >= self.scroll_offset + vp_height {
//             // Adjust so the current line is the last visible line in the viewport
//             self.scroll_offset = current_selection.saturating_sub(vp_height.saturating_sub(1));
//         }

//         // Ensure scroll_offset doesn't go beyond what's possible
//         if !self.lines.is_empty() {
//             let max_scroll_offset = self.lines.len().saturating_sub(vp_height);
//             self.scroll_offset = self.scroll_offset.min(max_scroll_offset);
//         } else {
//             self.scroll_offset = 0; // No lines, no scroll
//         }
//     }
// }

// fn main() -> Result<()> {
//     let args = Args::parse();
//     stdout().execute(EnterAlternateScreen)?;
//     enable_raw_mode()?;
//     let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
//     terminal.clear()?;

//     let mut app_state = AppState::new(args.file_path)?;

//     run_app(&mut terminal, &mut app_state)?;

//     disable_raw_mode()?;
//     stdout().execute(LeaveAlternateScreen)?;
//     Ok(())
// }

// fn run_app(
//     terminal: &mut Terminal<CrosstermBackend<Stdout>>,
//     // AppState needs to be mutable for ListState
//     app_state: &mut AppState,
// ) -> Result<()> {
//     loop {
//         let viewport_height = terminal.size()?.height as usize;
//         let content_height = viewport_height.saturating_sub(2); // Account for borders

//         // Adjust scroll AFTER potential state changes from input, BEFORE drawing
//         app_state.adjust_scroll(content_height);

//         // Draw UI, passing mutable app_state for ListState
//         terminal.draw(|frame| ui(frame, app_state))?;

//         // Event handling
//         if event::poll(Duration::from_millis(50))? {
//             if let Event::Key(key) = event::read()? {
//                 if key.kind == KeyEventKind::Press {
//                     match key.code {
//                         KeyCode::Char('q') | KeyCode::Esc => app_state.should_quit = true,
//                         // Call AppState methods which now update ListState
//                         KeyCode::Up | KeyCode::Char('k') => app_state.move_up(),
//                         KeyCode::Down | KeyCode::Char('j') => app_state.move_down(),
//                         KeyCode::PageUp => app_state.page_up(content_height),
//                         KeyCode::PageDown => app_state.page_down(content_height),
//                         KeyCode::Home => app_state.go_home(),
//                         KeyCode::End => app_state.go_end(),
//                         _ => {}
//                     }
//                     // Note: scroll adjustment happens at the start of the next loop iteration
//                 }
//             }
//         }

//         if app_state.should_quit {
//             break;
//         }
//     }
//     Ok(())
// }

// // ui function now takes mutable app_state because ListState needs it
// fn ui(frame: &mut Frame, app_state: &mut AppState) {
//     let area = frame.area();

//     // --- Syntax Highlighting Setup ---
//     let syntax = app_state
//         .syntax_set
//         .find_syntax_by_path(app_state.file_path.to_str().unwrap_or(""))
//         .or_else(|| {
//             app_state.syntax_set.find_syntax_by_extension(
//                 app_state
//                     .file_path
//                     .extension()
//                     .and_then(|s| s.to_str())
//                     .unwrap_or(""),
//             )
//         })
//         .unwrap_or_else(|| app_state.syntax_set.find_syntax_plain_text());

//     let mut highlighter = HighlightLines::new(syntax, &app_state.theme);

//     // --- Prepare List Items ---
//     let mut list_items: Vec<ListItem> = Vec::new();
//     let display_height = area.height.saturating_sub(2) as usize; // Height inside borders
//     let line_number_width = app_state.lines.len().to_string().len().max(3); // Min width for line numbers

//     // Extract the theme's background color
//     let theme_bg = app_state.theme.settings.background.map_or(
//         Color::Reset, // Fallback if theme has no background
//         |bg| Color::Rgb(bg.r, bg.g, bg.b),
//     );

//     // Iterate only over the lines that should be visible based on scroll_offset
//     for (i, line_content) in app_state
//         .lines
//         .iter()
//         .enumerate()
//         .skip(app_state.scroll_offset) // Start rendering from the scroll offset
//         .take(display_height)       // Render only enough lines to fill the viewport
//     {
//         // No need to track line_idx separately, `i` is the index within the visible slice

//         // 1. Create Line Number Span
//         let line_num = app_state.scroll_offset + i + 1; // Calculate the actual line number
//         let line_num_span = Span::styled(
//             // Use > for right alignment, pad with spaces
//             format!("{:<width$} ", line_num, width = line_number_width),
//             // Style for line numbers (dimmed foreground)
//             // The background will be handled by the List widget's style/highlight_style
//             Style::default().fg(Color::DarkGray),
//         );

//         // 2. Create Content Spans using Syntect highlighting
//         let ranges: Vec<(syntect::highlighting::Style, &str)> = highlighter
//             .highlight_line(line_content, &app_state.syntax_set)
//             .unwrap_or_else(|_| vec![(Default::default(), line_content)]); // Fallback

//         let content_spans: Vec<Span> = ranges
//             .into_iter()
//             .filter_map(|(syntect_style, content)| {
//                 // Convert syntect style to ratatui style
//                 // We don't manually apply background here anymore
//                 let ratatui_style = syntect_tui::translate_style(syntect_style).ok()?;
//                 Some(Span::styled(content.to_string(), ratatui_style))
//             })
//             .collect();

//         // 3. Combine Spans into a Line
//         let mut all_spans = vec![line_num_span];
//         all_spans.extend(content_spans);
//         let line = Line::from(all_spans); //.alignment(Alignment::Left); // Ensure left alignment

//         // 4. Create ListItem from the Line
//         list_items.push(ListItem::new(line));
//     }

//     // --- Create Widgets ---
//     let list_title = format!(
//         " File: {} (Line {}/{}) ",
//         app_state.file_path.file_name().unwrap_or_default().to_string_lossy(),
//         // Use the synced index for display
//         app_state.current_line_index + 1,
//         app_state.lines.len()
//     );

//     let list_block = Block::default()
//         .borders(Borders::ALL)
//         .title(list_title)
//         .title_alignment(Alignment::Center);

//     // Define the highlight style for the selected List item
//     let highlight_bg = Color::Rgb(35, 38, 46); // Example highlight color
//     // let highlight_bg = Color::DarkGray; // Alternative highlight
//     let list_highlight_style = Style::default().bg(highlight_bg);


//     // Create the List widget
//     let list_widget = List::new(list_items)
//         .block(list_block)
//         // Apply the theme background to the entire list area
//         .style(Style::default().bg(theme_bg))
//         .highlight_style(list_highlight_style);
//         // Optional: Add a symbol to the selected line
//         // .highlight_symbol("> ");

//     // --- Render ---
//     // Use render_stateful_widget with the ListState
//     frame.render_stateful_widget(list_widget, area, &mut app_state.list_state);
// }
use anyhow::{Context, Result};
use clap::Parser;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, List, ListItem, ListState},
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
};
use syntect_tui; // Use the syntect_tui bridge

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
    // current_line_index is only for display purposes now, derived from list_state
    current_line_index: usize,
    // scroll_offset is now managed internally by ListState
    list_state: ListState, // Use ListState for selection AND offset
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

        let syntax_set = SyntaxSet::load_defaults_newlines();
        let theme_set = ThemeSet::load_defaults();
        let theme = theme_set.themes["base16-ocean.dark"].clone();

        // Initialize ListState, selecting the first line
        let mut list_state = ListState::default();
        let initial_selection = 0;
        if !lines.is_empty() {
            list_state.select(Some(initial_selection));
        }

        Ok(AppState {
            file_path,
            lines,
            current_line_index: initial_selection, // Initialize display index
            // No scroll_offset field anymore
            list_state,
            should_quit: false,
            syntax_set,
            theme,
        })
    }

    // Helper to sync current_line_index for display after list_state changes
    fn sync_current_line_display(&mut self) {
        self.current_line_index = self.list_state.selected().unwrap_or(0);
    }

    // --- Movement methods update ListState.select ---
    // The list widget automatically adjusts the view (offset) based on selection

    fn move_up(&mut self) {
        if let Some(selected) = self.list_state.selected() {
            if selected > 0 {
                self.list_state.select(Some(selected - 1));
            }
        } else if !self.lines.is_empty() {
            self.list_state.select(Some(0)); // Select first if nothing was selected
        }
        self.sync_current_line_display();
    }

    fn move_down(&mut self) {
        if let Some(selected) = self.list_state.selected() {
            let max_index = self.lines.len().saturating_sub(1);
            if selected < max_index {
                self.list_state.select(Some(selected + 1));
            }
        } else if !self.lines.is_empty() {
             self.list_state.select(Some(0)); // Select first if nothing was selected
        }
        self.sync_current_line_display();
    }

     fn page_up(&mut self, viewport_height: usize) {
        if let Some(selected) = self.list_state.selected() {
            let jump = viewport_height.saturating_sub(1).max(1);
            let new_selection = selected.saturating_sub(jump);
            self.list_state.select(Some(new_selection));
        } else if !self.lines.is_empty() {
             self.list_state.select(Some(0));
        }
        self.sync_current_line_display();
    }

    fn page_down(&mut self, viewport_height: usize) {
        if let Some(selected) = self.list_state.selected() {
             let jump = viewport_height.saturating_sub(1).max(1);
             let max_index = self.lines.len().saturating_sub(1);
             let new_selection = selected.saturating_add(jump).min(max_index);
             self.list_state.select(Some(new_selection));
        } else if !self.lines.is_empty() {
             self.list_state.select(Some(0));
        }
        self.sync_current_line_display();
    }

    fn go_home(&mut self) {
        if !self.lines.is_empty() {
            self.list_state.select(Some(0));
            self.sync_current_line_display();
        }
    }

    fn go_end(&mut self) {
        if !self.lines.is_empty() {
            let last_index = self.lines.len() - 1;
            self.list_state.select(Some(last_index));
            self.sync_current_line_display();
        }
    }

    // No adjust_scroll method needed anymore!
}

fn main() -> Result<()> {
    let args = Args::parse();
    stdout().execute(EnterAlternateScreen)?;
    enable_raw_mode()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    terminal.clear()?;

    let mut app_state = AppState::new(args.file_path)?;

    run_app(&mut terminal, &mut app_state)?;

    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;
    Ok(())
}

fn run_app(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app_state: &mut AppState,
) -> Result<()> {
    loop {
        // Draw UI, passing mutable app_state for ListState
        terminal.draw(|frame| ui(frame, app_state))?;

        // Event handling
        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    // Need viewport height for page up/down calculations
                    let viewport_height = terminal.size()?.height as usize;
                    let content_height = viewport_height.saturating_sub(2); // Account for borders

                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => app_state.should_quit = true,
                        KeyCode::Up | KeyCode::Char('k') => app_state.move_up(),
                        KeyCode::Down | KeyCode::Char('j') => app_state.move_down(),
                        KeyCode::Char('u') | KeyCode::PageUp => app_state.page_up(content_height),
                        KeyCode::Char('d') |KeyCode::PageDown => app_state.page_down(content_height),
                        KeyCode::Home => app_state.go_home(),
                        KeyCode::End => app_state.go_end(),
                        _ => {}
                    }
                    // No need to call adjust_scroll here
                }
            }
        }

        if app_state.should_quit {
            break;
        }
    }
    Ok(())
}

fn ui(frame: &mut Frame, app_state: &mut AppState) {
    let area = frame.area();

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

    // --- Prepare ONLY Visible List Items ---
    let mut list_items: Vec<ListItem> = Vec::new();
    let display_height = area.height.saturating_sub(2) as usize; // Height inside borders
    let line_number_width = app_state.lines.len().to_string().len().max(3); // Min width

    // *** Get the current scroll offset from ListState ***
    let current_offset = app_state.list_state.offset();

    // Extract the theme's background color
    let theme_bg = app_state.theme.settings.background.map_or(
        Color::Reset, // Fallback
        |bg| Color::Rgb(bg.r, bg.g, bg.b),
    );

    // Iterate only over the lines that should be visible based on ListState's offset
    // Ensure we don't take more lines than available
    let visible_line_count = display_height.min(app_state.lines.len().saturating_sub(current_offset));

    for i in 0..visible_line_count {
        // Calculate the absolute index of the line in the full file content
        let line_index = current_offset + i;
        // Get the line content safely
        if let Some(line_content) = app_state.lines.get(line_index) {
            // 1. Create Line Number Span
            let line_num = line_index + 1; // 1-based line number
            let line_num_span = Span::styled(
                format!("{:<width$} ", line_num, width = line_number_width),
                Style::default().fg(Color::DarkGray), // BG handled by List
            );

            // 2. Create Content Spans using Syntect highlighting
            let ranges: Vec<(syntect::highlighting::Style, &str)> = highlighter
                .highlight_line(line_content, &app_state.syntax_set)
                .unwrap_or_else(|_| vec![(Default::default(), line_content)]);

            let content_spans: Vec<Span> = ranges
                .into_iter()
                .filter_map(|(syntect_style, content)| {
                    let ratatui_style = syntect_tui::translate_style(syntect_style).ok()?;
                    Some(Span::styled(content.to_string(), ratatui_style))
                })
                .collect();

            // 3. Combine Spans into a Line
            let mut all_spans = vec![line_num_span];
            all_spans.extend(content_spans);
            let line = Line::from(all_spans);

            // 4. Create ListItem from the Line
            list_items.push(ListItem::new(line));
        }
    }

    // --- Create Widgets ---
    let list_title = format!(
        " File: {} (Line {}/{}) ",
        app_state.file_path.file_name().unwrap_or_default().to_string_lossy(),
        app_state.current_line_index + 1, // Use the synced display index
        app_state.lines.len()
    );

    let list_block = Block::default()
        .borders(Borders::ALL)
        .title(list_title)
        .title_alignment(Alignment::Center);

    // Define the highlight style for the selected List item
    let highlight_bg = Color::Rgb(35, 38, 46); // Example highlight color
    let list_highlight_style = Style::default().bg(highlight_bg);


    // Create the List widget using only the visible items
    let list_widget = List::new(list_items)
        .block(list_block)
        .style(Style::default().bg(theme_bg)) // Theme background for the whole list area
        .highlight_style(list_highlight_style);
        // Optional: Add a symbol to the selected line
        // .highlight_symbol("> ");

    // --- Render ---
    // Render the list statefully. This function will use list_state.selected()
    // and list_state.offset() to render correctly and will update the offset
    // if the selection moves out of the visible area.
    frame.render_stateful_widget(list_widget, area, &mut app_state.list_state);
}
