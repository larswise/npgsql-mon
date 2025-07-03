use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    text::Line,
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
};

use sqlformat::{FormatOptions, QueryParams, format};
use std::{collections::HashSet, sync::mpsc, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    net::TcpListener,
};

use arboard::Clipboard;
mod format;
mod ui;

#[derive(serde::Serialize, serde::Deserialize, Debug)]
struct SqlLogMessage {
    statement: String,
    duration: u64,
    timestamp: String,                // or use chrono::DateTime if needed
    endpoint: Option<String>,         // nullable field
    http_method: Option<String>,      // nullable field
    caller_namespace: Option<String>, // nullable field
    caller_class: Option<String>,     // nullable field
    caller_method: Option<String>,    // nullable field
    uid: Option<String>,              // unique identifier for tracking selections
}

const MAX_EXPANDED_HEIGHT: usize = 40; // Maximum lines for expanded accordion (increased for 80% screen usage)

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let (tx, rx) = mpsc::channel::<String>();

    // Spawn TCP listener thread
    tokio::spawn(async move {
        let listener = TcpListener::bind("localhost:6000").await.unwrap();
        loop {
            let (socket, _) = listener.accept().await.unwrap();
            let reader = BufReader::new(socket);
            let mut lines = reader.lines();

            while let Ok(Some(line)) = lines.next_line().await {
                if tx.send(line).is_err() {
                    break;
                }
            }
        }
    });

    // Start TUI loop
    run_tui(rx)?;
    Ok(())
}

fn run_tui(rx: mpsc::Receiver<String>) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut log_lines: Vec<SqlLogMessage> = vec![];
    let mut expanded_uids: HashSet<String> = HashSet::new();
    let mut list_state = ListState::default();
    list_state.select(Some(1)); // Start at index 1 to account for padding line

    // Scroll state management
    let mut scroll_mode = false;
    let mut scroll_offsets: std::collections::HashMap<usize, usize> =
        std::collections::HashMap::new(); // Keyed by actual_index
    let mut scroll_cursors: std::collections::HashMap<usize, usize> =
        std::collections::HashMap::new(); // Keyed by actual_index

    // Persistent clipboard to avoid "dropped too quickly" warning
    let mut clipboard = Clipboard::new().ok();

    // Visual feedback for copying (vim-style flash)
    let mut copy_flash_state: Option<(usize, std::time::Instant)> = None;
    const COPY_FLASH_DURATION: std::time::Duration = std::time::Duration::from_millis(200);

    // Filter state
    let mut filter_text = String::new();
    let mut filter_focused = false;

    // UID-based selection tracking
    let mut selected_uid: Option<String> = None;

    // Track the last known list height for paging
    let mut last_list_height = 10usize;
    loop {
        // Store current selection UID before processing new logs
        if let Some(selected) = list_state.selected() {
            if selected > 0 {
                let actual_index = selected - 1;
                let filtered_lines = filter_log_lines(&log_lines, &filter_text);
                if actual_index < filtered_lines.len() {
                    let line = filtered_lines[filtered_lines.len() - 1 - actual_index];
                    selected_uid = line.uid.clone();
                }
            }
        }

        // Check for new logs
        let mut new_logs_received = false;
        while let Ok(line) = rx.try_recv() {
            let mut msg: SqlLogMessage = serde_json::from_str(&line)?;
            // Generate UID if not present
            if msg.uid.is_none() {
                msg.uid = Some(format!("{}-{}", msg.timestamp, log_lines.len()));
            }
            log_lines.push(msg);
            if log_lines.len() > 1000 {
                log_lines.remove(0);
            }
            new_logs_received = true;
        }

        // Restore selection based on UID after new logs arrive
        // Only do this if scroll_mode is NOT active, so scroll mode selection stays stable
        if new_logs_received && selected_uid.is_some() {
            if !scroll_mode {
                let filtered_lines = filter_log_lines(&log_lines, &filter_text);
                if let Some(uid) = &selected_uid {
                    // Find the item with the matching UID
                    for (index, line) in filtered_lines.iter().rev().enumerate() {
                        if line.uid.as_ref() == Some(uid) {
                            list_state.select(Some(index + 1)); // +1 for padding line
                            break;
                        }
                    }
                }
            }
        }

        // Check and clear flash state if duration has passed
        if let Some((_, flash_time)) = copy_flash_state {
            if flash_time.elapsed() > COPY_FLASH_DURATION {
                copy_flash_state = None;
            }
        }

        // Draw UI
        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(3), Constraint::Min(0)].as_ref())
                .split(f.size());

            // Save the height for paging (use the list area height)
            last_list_height = chunks[1].height as usize;

            // Render filter input
            let filter_input = Paragraph::new(filter_text.clone())
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(if filter_focused {
                            Style::default().fg(Color::Yellow)
                        } else {
                            Style::default().fg(Color::Gray)
                        })
                        .title(" Filter requests ")
                        .title_style(Style::default().fg(Color::White)),
                )
                .style(Style::default().fg(Color::White));

            f.render_widget(filter_input, chunks[0]);

            // Create inner padding area inside the border
            let inner_area = ratatui::layout::Rect {
                x: chunks[1].x + 1, // Reduced horizontal padding inside border
                y: chunks[1].y + 1, // Reduced vertical padding inside border
                width: chunks[1].width.saturating_sub(2), // Reduce width for padding
                height: chunks[1].height.saturating_sub(1), // Reduce height for padding
            };

            // Create items for the accordion list with top padding
            let mut items: Vec<ListItem> = vec![
                // Add empty line for top padding inside the border
                ListItem::new(vec![Line::from("")]),
            ];

            // Filter the log lines based on the filter text
            let filtered_lines = filter_log_lines(&log_lines, &filter_text);

            // Add the actual accordion items
            let accordion_items: Vec<ListItem> = filtered_lines
                .iter()
                .rev()
                .enumerate()
                .map(|(index, line)| {
                    ui::render_accordion_item(
                        index,
                        line,
                        &expanded_uids,
                        copy_flash_state,
                        &list_state,
                        scroll_mode,
                        &scroll_offsets,
                        &scroll_cursors,
                        MAX_EXPANDED_HEIGHT,
                        chunks[0].width.saturating_sub(2) as usize,
                    )
                })
                .collect();

            items.extend(accordion_items);

            let log_list = List::new(items)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::Rgb(0, 149, 255))) // #0095ff
                        .title(" Npgsql monitor ")
                        .title_style(Style::default().fg(Color::White)),
                )
                .highlight_style(Style::default())
                .highlight_symbol("► ");

            f.render_stateful_widget(log_list, inner_area, &mut list_state);
        })?;

        // Handle keyboard events
        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key_event) = event::read()? {
                use crossterm::event::KeyEventKind;
                // Only process key press events, not releases or repeats
                if key_event.kind == KeyEventKind::Press {
                    let key = key_event;
                    if scroll_mode {
                        // Handle scroll mode keys
                        match key.code {
                            KeyCode::Char('q') => break,
                            KeyCode::Char('h') => {
                                scroll_mode = false;
                            }
                            KeyCode::Char('j') | KeyCode::Down => {
                                if let Some(selected) = list_state.selected() {
                                    if selected > 0 {
                                        let actual_index = selected - 1;
                                        let filtered_lines =
                                            filter_log_lines(&log_lines, &filter_text);
                                        ui::handle_down(
                                            &filtered_lines,
                                            &list_state,
                                            &mut scroll_offsets,
                                            &mut scroll_cursors,
                                        );
                                    }
                                }
                            }
                            KeyCode::Char('k') | KeyCode::Up => {
                                if let Some(selected) = list_state.selected() {
                                    if selected > 0 {
                                        let actual_index = selected - 1;
                                        let current_cursor =
                                            scroll_cursors.get(&actual_index).cloned().unwrap_or(0);
                                        let current_offset =
                                            scroll_offsets.get(&actual_index).cloned().unwrap_or(0);

                                        // Move cursor up if not at the top
                                        if current_cursor > 0 {
                                            let new_cursor = current_cursor - 1;
                                            scroll_cursors.insert(actual_index, new_cursor);

                                            // Auto-scroll if cursor goes above visible area
                                            if new_cursor < current_offset {
                                                scroll_offsets
                                                    .insert(actual_index, current_offset - 1);
                                            }
                                        }
                                    }
                                }
                            }
                            KeyCode::Char('d')
                                if key.modifiers == crossterm::event::KeyModifiers::CONTROL =>
                            {
                                if let Some(selected) = list_state.selected() {
                                    if selected > 0 {
                                        let actual_index = selected - 1;
                                        // Page down (Ctrl+d) - move cursor down by half a page
                                        let current_cursor =
                                            scroll_cursors.get(&actual_index).cloned().unwrap_or(0);
                                        let current_offset =
                                            scroll_offsets.get(&actual_index).cloned().unwrap_or(0);
                                        let page_size = MAX_EXPANDED_HEIGHT / 2; // Half page like vim

                                        // Use the same logic as the j/k scrolling to calculate total lines
                                        if actual_index < log_lines.len() {
                                            let line =
                                                &log_lines[log_lines.len() - 1 - actual_index];
                                            let mut total_lines = 0;

                                            if line.statement.contains("[-- Batch Command") {
                                                // Count batch processing lines
                                                let mut current_batch_sql = String::new();
                                                for statement_line in line.statement.lines() {
                                                    if statement_line
                                                        .starts_with("[-- Batch Command")
                                                    {
                                                        if !current_batch_sql.trim().is_empty() {
                                                            total_lines += 1; // batch header
                                                            let format_options = FormatOptions {
                                                                indent: sqlformat::Indent::Spaces(
                                                                    2,
                                                                ),
                                                                uppercase: Some(false),
                                                                lines_between_queries: 1,
                                                                ignore_case_convert: Some(vec![]),
                                                            };
                                                            let formatted_sql = format(
                                                                &current_batch_sql.trim(),
                                                                &QueryParams::None,
                                                                &format_options,
                                                            );
                                                            if formatted_sql.trim().is_empty() {
                                                                total_lines += current_batch_sql
                                                                    .lines()
                                                                    .count()
                                                                    .max(1);
                                                            } else {
                                                                total_lines +=
                                                                    formatted_sql.lines().count();
                                                            }
                                                            total_lines += 1; // separator
                                                        }
                                                        current_batch_sql.clear();
                                                    } else {
                                                        if !current_batch_sql.is_empty() {
                                                            current_batch_sql.push('\n');
                                                        }
                                                        current_batch_sql.push_str(statement_line);
                                                    }
                                                }
                                                // Final batch
                                                if !current_batch_sql.trim().is_empty() {
                                                    total_lines += 1; // batch header
                                                    let format_options = FormatOptions {
                                                        indent: sqlformat::Indent::Spaces(2),
                                                        uppercase: Some(false),
                                                        lines_between_queries: 1,
                                                        ignore_case_convert: Some(vec![]),
                                                    };
                                                    let formatted_sql = format(
                                                        &current_batch_sql.trim(),
                                                        &QueryParams::None,
                                                        &format_options,
                                                    );
                                                    if formatted_sql.trim().is_empty() {
                                                        total_lines += current_batch_sql
                                                            .lines()
                                                            .count()
                                                            .max(1);
                                                    } else {
                                                        total_lines +=
                                                            formatted_sql.lines().count();
                                                    }
                                                }
                                            } else {
                                                // Regular statement
                                                let format_options = FormatOptions {
                                                    indent: sqlformat::Indent::Spaces(2),
                                                    uppercase: Some(false),
                                                    lines_between_queries: 1,
                                                    ignore_case_convert: Some(vec![]),
                                                };
                                                let formatted_sql = format(
                                                    &line.statement,
                                                    &QueryParams::None,
                                                    &format_options,
                                                );
                                                if formatted_sql.trim().is_empty() {
                                                    total_lines +=
                                                        line.statement.lines().count().max(1);
                                                } else {
                                                    total_lines += formatted_sql.lines().count();
                                                }
                                                total_lines += 1; // end statement marker
                                            }

                                            // Move cursor down by half page
                                            let new_cursor = std::cmp::min(
                                                current_cursor + page_size,
                                                total_lines.saturating_sub(1),
                                            );
                                            scroll_cursors.insert(actual_index, new_cursor);

                                            // Auto-scroll if cursor goes beyond visible area
                                            if new_cursor >= current_offset + MAX_EXPANDED_HEIGHT {
                                                let new_offset = std::cmp::min(
                                                    current_offset + page_size,
                                                    total_lines.saturating_sub(MAX_EXPANDED_HEIGHT),
                                                );
                                                scroll_offsets.insert(actual_index, new_offset);
                                            }
                                        }
                                    }
                                }
                            }
                            KeyCode::Char('u')
                                if key.modifiers == crossterm::event::KeyModifiers::CONTROL =>
                            {
                                if let Some(selected) = list_state.selected() {
                                    if selected > 0 {
                                        let actual_index = selected - 1;
                                        // Page up (Ctrl+u) - move cursor up by half a page
                                        let current_cursor =
                                            scroll_cursors.get(&actual_index).cloned().unwrap_or(0);
                                        let current_offset =
                                            scroll_offsets.get(&actual_index).cloned().unwrap_or(0);
                                        let page_size = MAX_EXPANDED_HEIGHT / 2; // Half page like vim

                                        // Move cursor up by half page
                                        let new_cursor = current_cursor.saturating_sub(page_size);
                                        scroll_cursors.insert(actual_index, new_cursor);

                                        // Auto-scroll if cursor goes above visible area
                                        if new_cursor < current_offset {
                                            let new_offset =
                                                current_offset.saturating_sub(page_size);
                                            scroll_offsets.insert(actual_index, new_offset);
                                        }
                                    }
                                }
                            }
                            KeyCode::Char('y') => {
                                if let Some(selected) = list_state.selected() {
                                    if selected > 0 {
                                        let actual_index = selected - 1;
                                        if actual_index < log_lines.len() {
                                            let line =
                                                &log_lines[log_lines.len() - 1 - actual_index];
                                            let cursor_pos = scroll_cursors
                                                .get(&actual_index)
                                                .cloned()
                                                .unwrap_or(0);

                                            let text_to_copy =
                                                if line.statement.contains("[-- Batch Command") {
                                                    format::extract_batch_statement_at_cursor(
                                                        &line.statement,
                                                        cursor_pos,
                                                    )
                                                } else {
                                                    let format_options = FormatOptions {
                                                        indent: sqlformat::Indent::Spaces(2),
                                                        uppercase: Some(false),
                                                        lines_between_queries: 1,
                                                        ignore_case_convert: Some(vec![]),
                                                    };
                                                    let formatted_sql = format(
                                                        &line.statement,
                                                        &QueryParams::None,
                                                        &format_options,
                                                    );
                                                    if formatted_sql.trim().is_empty() {
                                                        line.statement.clone()
                                                    } else {
                                                        formatted_sql
                                                    }
                                                };

                                            if let Some(ref mut cb) = clipboard {
                                                if cb.set_text(text_to_copy).is_ok() {
                                                    // Flash the indicator on the correct item (use actual_index for rendering)
                                                    copy_flash_state = Some((
                                                        actual_index,
                                                        std::time::Instant::now(),
                                                    ));
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    } else if filter_focused {
                        // Handle filter input
                        match key.code {
                            KeyCode::Char('q') => break,
                            KeyCode::Esc => {
                                filter_focused = false;
                            }
                            KeyCode::Char(c) => {
                                filter_text.push(c);
                            }
                            KeyCode::Backspace => {
                                filter_text.pop();
                            }
                            KeyCode::Enter => {
                                filter_focused = false;
                            }
                            _ => {}
                        }
                    } else {
                        // Handle normal accordion navigation
                        match key.code {
                            KeyCode::Char('q') => break,
                            KeyCode::Char('f') => {
                                filter_focused = true;
                            }
                            KeyCode::Up | KeyCode::Char('k') => {
                                if let Some(selected) = list_state.selected() {
                                    if selected > 1 {
                                        // Skip the padding line at index 0
                                        list_state.select(Some(selected - 1));
                                    }
                                }
                            }
                            KeyCode::Down | KeyCode::Char('j') => {
                                let filtered_lines = filter_log_lines(&log_lines, &filter_text);
                                if let Some(selected) = list_state.selected() {
                                    if selected < filtered_lines.len() {
                                        // Account for padding line
                                        list_state.select(Some(selected + 1));
                                    }
                                } else if !filtered_lines.is_empty() {
                                    list_state.select(Some(1)); // Start at index 1 (first actual item)
                                }
                            }
                            KeyCode::Char('d')
                                if key.modifiers == crossterm::event::KeyModifiers::CONTROL =>
                            {
                                // Page down (Ctrl+d) - move selection down by half a page
                                if let Some(selected) = list_state.selected() {
                                    let page_size =
                                        (last_list_height.saturating_sub(2) / 2) as usize; // half page, minus padding
                                    let max_index = log_lines.len();
                                    let new_selected =
                                        std::cmp::min(selected + page_size, max_index);
                                    if new_selected > 0 && new_selected <= max_index {
                                        list_state.select(Some(new_selected));
                                    }
                                }
                            }
                            KeyCode::Char('u')
                                if key.modifiers == crossterm::event::KeyModifiers::CONTROL =>
                            {
                                // Page up (Ctrl+u) - move selection up by half a page
                                if let Some(selected) = list_state.selected() {
                                    let page_size =
                                        (last_list_height.saturating_sub(2) / 2) as usize; // half page, minus padding
                                    let new_selected = selected.saturating_sub(page_size);
                                    if new_selected > 0 {
                                        list_state.select(Some(new_selected));
                                    } else {
                                        list_state.select(Some(1)); // Don't go above first item
                                    }
                                }
                            }
                            KeyCode::Enter => {
                                if let Some(selected) = list_state.selected() {
                                    if selected > 0 {
                                        let actual_index = selected - 1; // Convert to actual log index
                                        let filtered_lines =
                                            filter_log_lines(&log_lines, &filter_text);
                                        if actual_index < filtered_lines.len() {
                                            if let Some(uid) = &filtered_lines
                                                [filtered_lines.len() - 1 - actual_index]
                                                .uid
                                            {
                                                if expanded_uids.contains(uid) {
                                                    // Collapse the accordion
                                                    expanded_uids.remove(uid);
                                                } else {
                                                    // Expand the accordion
                                                    expanded_uids.insert(uid.clone());
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            KeyCode::Char('l') => {
                                if let Some(selected) = list_state.selected() {
                                    if selected > 0 {
                                        let actual_index = selected - 1;
                                        let filtered_lines =
                                            filter_log_lines(&log_lines, &filter_text);
                                        if actual_index < filtered_lines.len() {
                                            if let Some(uid) = &filtered_lines
                                                [filtered_lines.len() - 1 - actual_index]
                                                .uid
                                            {
                                                if expanded_uids.contains(uid) {
                                                    scroll_mode = true;
                                                    // Always reset scroll position when entering scroll mode
                                                    scroll_offsets.insert(actual_index, 0);
                                                    scroll_cursors.insert(actual_index, 0);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    // Clean up terminal
    disable_raw_mode()?;
    execute!(std::io::stdout(), LeaveAlternateScreen)?;
    Ok(())
}

fn lerp(a: u8, b: u8, t: f64) -> u8 {
    ((a as f64) + (b as f64 - a as f64) * t).round() as u8
}

fn format_duration(ms: u64) -> String {
    if ms < 1000 {
        format!("{:>3}ms", ms)
    } else {
        format!("{:.3}s", ms as f64 / 1000.0)
    }
}

fn interpolate_color(ms: u64) -> (u8, u8, u8) {
    let green = (38, 255, 0);
    let yellow = (255, 252, 66);
    let red = (237, 83, 83);

    if ms < 250 {
        let t = ms as f64 / 500.0;
        (
            lerp(green.0, yellow.0, t),
            lerp(green.1, yellow.1, t),
            lerp(green.2, yellow.2, t),
        )
    } else if ms < 5000 {
        let t = (ms.saturating_sub(500)) as f64 / (5000.0 - 500.0);
        (
            lerp(yellow.0, red.0, t),
            lerp(yellow.1, red.1, t),
            lerp(yellow.2, red.2, t),
        )
    } else {
        red
    }
}

fn get_http_method_color(method: &str) -> Color {
    match method.to_uppercase().as_str() {
        "GET" => Color::Rgb(97, 175, 254),     // Blue (#61affe)
        "POST" => Color::Rgb(73, 204, 144),    // Green (#49cc90)
        "PUT" => Color::Rgb(252, 161, 48),     // Orange (#fca130)
        "DELETE" => Color::Rgb(249, 62, 62),   // Red (#f93e3e)
        "PATCH" => Color::Rgb(80, 227, 194),   // Teal (#50e3c2)
        "OPTIONS" => Color::Rgb(144, 18, 254), // Purple (#9012fe)
        "HEAD" => Color::Rgb(155, 155, 155),   // Grey (#9b9b9b)
        _ => Color::Rgb(128, 128, 128),        // Default grey
    }
}

fn filter_log_lines<'a>(
    log_lines: &'a [SqlLogMessage],
    filter_text: &str,
) -> Vec<&'a SqlLogMessage> {
    if filter_text.is_empty() {
        return log_lines.iter().collect();
    }

    log_lines
        .iter()
        .filter(|line| {
            // Check http_method or "CALL" when http_method is null
            let method_match = if line.http_method.is_none() {
                "CALL".contains(filter_text)
            } else {
                line.http_method
                    .as_ref()
                    .map_or(false, |method| method.contains(filter_text))
            };

            // Check endpoint
            let endpoint_match = line
                .endpoint
                .as_ref()
                .map_or(false, |endpoint| endpoint.contains(filter_text));

            // Check caller_class
            let caller_class_match = line
                .caller_class
                .as_ref()
                .map_or(false, |class| class.contains(filter_text));

            // Check caller_method
            let caller_method_match = line
                .caller_method
                .as_ref()
                .map_or(false, |method| method.contains(filter_text));

            method_match || endpoint_match || caller_class_match || caller_method_match
        })
        .collect()
}
