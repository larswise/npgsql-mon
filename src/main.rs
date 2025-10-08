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

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
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

// Group key for organizing messages by endpoint + HTTP method
#[derive(Hash, Eq, PartialEq, Clone, Debug)]
struct RequestGroup {
    endpoint: String,
    http_method: String,
}

impl RequestGroup {
    fn from_message(msg: &SqlLogMessage) -> Self {
        let (endpoint_str, http_method) = if msg.http_method.is_none() {
            // Show caller info when http_method is null
            let caller_info = match (&msg.caller_method, &msg.caller_class) {
                (Some(method), Some(class)) => format!("{} in {}", method, class),
                (Some(method), None) => method.clone(),
                (None, Some(class)) => format!("in {}", class),
                (None, None) => "N/A".to_string(),
            };
            (caller_info, "CALL".to_string())
        } else {
            (
                msg.endpoint.clone().unwrap_or("N/A".to_string()),
                msg.http_method.clone().unwrap_or("UNKNOWN".to_string()),
            )
        };
        
        RequestGroup {
            endpoint: endpoint_str,
            http_method,
        }
    }
}

// Grouped data structure
struct GroupedLogMessages {
    groups: Vec<(RequestGroup, Vec<SqlLogMessage>)>,
}

impl GroupedLogMessages {
    fn from_messages(messages: &[SqlLogMessage], pinned_groups: &HashSet<RequestGroup>) -> Self {
        let mut group_map: std::collections::HashMap<RequestGroup, Vec<SqlLogMessage>> = 
            std::collections::HashMap::new();
            
        // Group messages by RequestGroup
        for msg in messages {
            let group = RequestGroup::from_message(msg);
            group_map.entry(group).or_insert_with(Vec::new).push(msg.clone());
        }
        
        // Convert to ordered vector, sorted by most recent message in each group
        let mut groups: Vec<(RequestGroup, Vec<SqlLogMessage>)> = group_map.into_iter().collect();
        groups.sort_by(|a, b| {
            // Pinned groups always come first
            let a_pinned = pinned_groups.contains(&a.0);
            let b_pinned = pinned_groups.contains(&b.0);
            
            match (a_pinned, b_pinned) {
                (true, false) => std::cmp::Ordering::Less,  // a is pinned, b is not
                (false, true) => std::cmp::Ordering::Greater, // b is pinned, a is not
                _ => {
                    // Both pinned or both not pinned, sort by timestamp
                    let a_latest = a.1.iter().map(|msg| &msg.timestamp).max();
                    let b_latest = b.1.iter().map(|msg| &msg.timestamp).max();
                    let timestamp_cmp = b_latest.cmp(&a_latest); // Most recent first
                    
                    // If timestamps are equal, use endpoint and method for stable sorting
                    if timestamp_cmp == std::cmp::Ordering::Equal {
                        let endpoint_cmp = a.0.endpoint.cmp(&b.0.endpoint);
                        if endpoint_cmp == std::cmp::Ordering::Equal {
                            a.0.http_method.cmp(&b.0.http_method)
                        } else {
                            endpoint_cmp
                        }
                    } else {
                        timestamp_cmp
                    }
                }
            }
        });
        
        GroupedLogMessages { groups }
    }
    
    #[allow(dead_code)]
    fn total_item_count(&self) -> usize {
        self.groups.iter().map(|(_, messages)| messages.len()).sum()
    }
}

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
    let mut log_buffer: Vec<String> = vec![]; // Buffer for new logs during scrollmode
    let mut expanded_uids: HashSet<String> = HashSet::new();
    let mut expanded_groups: HashSet<RequestGroup> = HashSet::new(); // Track expanded groups
    let mut pinned_groups: HashSet<RequestGroup> = HashSet::new(); // Track pinned groups
    let mut list_state = ListState::default();
    list_state.select(Some(1)); // Start at index 1 to account for padding line

    // Main scroll offset: index of the topmost visible item in the filtered list
    let mut main_scroll_offset: usize = 0;

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

    // Help screen state
    let mut help_screen_visible = false;

    // UID-based selection tracking
    let mut selected_uid: Option<String> = None;

    // Track the last known list height for paging
    let mut last_list_height = 10usize;
    loop {
        // Store current selection UID before processing new logs
        if let Some(selected) = list_state.selected() {
            if selected > 0 {
                let actual_index = selected - 1;
                let grouped_messages = GroupedLogMessages::from_messages(&log_lines, &pinned_groups);
                let flat_items = create_flat_navigation_structure(&grouped_messages, &expanded_groups, &filter_text);
                if actual_index < flat_items.len() {
                    match &flat_items[actual_index] {
                        FlatNavigationItem::Message(msg) => {
                            selected_uid = msg.uid.clone();
                        }
                        FlatNavigationItem::GroupHeader(_) => {
                            // Group headers don't have UIDs, keep the current selection
                        }
                    }
                }
            }
        }

        // Check for new logs
        let mut new_logs_received = false;
        while let Ok(line) = rx.try_recv() {
            if scroll_mode {
                log_buffer.push(line);
            } else {
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
        }
        // If scroll_mode was just exited, flush buffer
        if !scroll_mode && !log_buffer.is_empty() {
            for line in log_buffer.drain(..) {
                let mut msg: SqlLogMessage = serde_json::from_str(&line)?;
                if msg.uid.is_none() {
                    msg.uid = Some(format!("{}-{}", msg.timestamp, log_lines.len()));
                }
                log_lines.push(msg);
                if log_lines.len() > 1000 {
                    log_lines.remove(0);
                }
                new_logs_received = true;
            }
        }

        // Restore selection based on UID after new logs arrive
        // Only do this if scroll_mode is NOT active, so scroll mode selection stays stable
        if new_logs_received && selected_uid.is_some() {
            if !scroll_mode {
                let grouped_messages = GroupedLogMessages::from_messages(&log_lines, &pinned_groups);
                let flat_items = create_flat_navigation_structure(&grouped_messages, &expanded_groups, &filter_text);
                if let Some(uid) = &selected_uid {
                    // Find the item with the matching UID in the flattened structure
                    let mut found_index = None;
                    for (index, item) in flat_items.iter().enumerate() {
                        if let FlatNavigationItem::Message(msg) = item {
                            if msg.uid.as_ref() == Some(uid) {
                                found_index = Some(index);
                                list_state.select(Some(index + 1)); // +1 for padding line
                                break;
                            }
                        }
                    }
                    // Adjust main_scroll_offset to keep selected item at same visible position
                    if let Some(found_index) = found_index {
                        // If the previous selected index was known, keep the same relative position
                        // Otherwise, keep the selected item visible
                        let visible_height = last_list_height.saturating_sub(2); // minus border/padding
                        if found_index < main_scroll_offset {
                            main_scroll_offset = found_index;
                        } else if found_index >= main_scroll_offset + visible_height {
                            main_scroll_offset = found_index.saturating_sub(visible_height - 1);
                        }
                        // Clamp scroll offset to valid range
                        let max_scroll = flat_items.len().saturating_sub(visible_height);
                        if main_scroll_offset > max_scroll {
                            main_scroll_offset = max_scroll;
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
            if help_screen_visible {
                // Render help screen
                let help_text = vec![
                    Line::from(""),
                    Line::from("NPGSQL MONITOR - HOTKEYS"),
                    Line::from(""),
                    Line::from("Navigation:"),
                    Line::from("  j / ↓      Move down"),
                    Line::from("  k / ↑      Move up"),
                    Line::from("  Ctrl+d     Page down"),
                    Line::from("  Ctrl+u     Page up"),
                    Line::from(""),
                    Line::from("Actions:"),
                    Line::from("  Enter      Toggle expand/collapse"),
                    Line::from("  l          Enter scroll mode"),
                    Line::from("  t          Pin/unpin group"),
                    Line::from("  f          Focus filter"),
                    Line::from("  y          Copy SQL (in scroll mode)"),
                    Line::from("  h          Show this help"),
                    Line::from(""),
                    Line::from("Scroll Mode:"),
                    Line::from("  j / ↓      Scroll down one line"),
                    Line::from("  k / ↑      Scroll up one line"),
                    Line::from("  Ctrl+d     Scroll down half page"),
                    Line::from("  Ctrl+u     Scroll up half page"),
                    Line::from("  h          Exit scroll mode"),
                    Line::from("  y          Copy current SQL"),
                    Line::from("  Esc        Exit scroll mode & collapse"),
                    Line::from(""),
                    Line::from("Filter Mode:"),
                    Line::from("  Type       Filter by endpoint/method/class"),
                    Line::from("  Enter/Esc  Exit filter mode"),
                    Line::from(""),
                    Line::from("General:"),
                    Line::from("  q          Quit application"),
                    Line::from("  Esc        Close help screen"),
                    Line::from(""),
                ];

                let help_paragraph = Paragraph::new(help_text)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(Color::Yellow))
                            .title(" Help - Press Esc to return ")
                            .title_style(Style::default().fg(Color::Yellow)),
                    )
                    .style(Style::default().fg(Color::White));

                f.render_widget(help_paragraph, f.size());
            } else {
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints(
                        [
                            Constraint::Length(3), // filter
                            Constraint::Length(2), // indicator
                            Constraint::Min(0),    // accordion
                        ]
                        .as_ref(),
                    )
                    .split(f.size());

                // Save the height for paging (use the list area height)
                last_list_height = chunks[2].height as usize;

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

                // Calculate indicator state
                let _filtered_lines = filter_log_lines(&log_lines, &filter_text);
                let _visible_height = last_list_height.saturating_sub(2); // minus border/padding
                let above_count = main_scroll_offset;
                let indicator = if above_count > 0 {
                    Paragraph::new(format!("↑ {above_count} more items above"))
                        .style(Style::default().fg(Color::Yellow))
                } else {
                    Paragraph::new("↓ All items visible").style(Style::default().fg(Color::Green))
                };
                f.render_widget(indicator, chunks[1]);

                // Create inner padding area inside the border
                let inner_area = ratatui::layout::Rect {
                    x: chunks[2].x + 1, // Reduced horizontal padding inside border
                    y: chunks[2].y + 1, // Reduced vertical padding inside border
                    width: chunks[2].width.saturating_sub(2), // Reduce width for padding
                    height: chunks[2].height.saturating_sub(1), // Reduce height for padding
                };

                // Create items for the accordion list with top padding
                let mut items: Vec<ListItem> = vec![
                    // Add empty line for top padding inside the border
                    ListItem::new(vec![Line::from("")]),
                ];

                // Create grouped messages from the log lines
                let grouped_messages = GroupedLogMessages::from_messages(&log_lines, &pinned_groups);
                
                // Calculate dynamic max expanded height based on available screen space
                // Reserve space for at least one more log entry (minimum 5 lines for context)
                let min_reserved_space = 5; // Space for next log entry + separators
                let available_height = last_list_height.saturating_sub(4); // Account for borders/padding
                let dynamic_max_expanded_height = available_height.saturating_sub(min_reserved_space).max(10); // Minimum 10 lines for expanded content
                
                // Render grouped accordions
                let accordion_items = ui::render_grouped_accordions(
                    &grouped_messages,
                    &expanded_groups,
                    &expanded_uids,
                    copy_flash_state,
                    &list_state,
                    scroll_mode,
                    &scroll_offsets,
                    &scroll_cursors,
                    dynamic_max_expanded_height,
                    chunks[0].width.saturating_sub(2) as usize,
                    &filter_text,
                    &pinned_groups,
                );

                items.extend(accordion_items);

                let log_list = List::new(items)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(Color::Rgb(0, 149, 255))) // #0095ff
                            .title(" Postgresql query monitor ")
                            .title_style(Style::default().fg(Color::White)),
                    )
                    .highlight_style(Style::default())
                    .highlight_symbol("► ");

                f.render_stateful_widget(log_list, inner_area, &mut list_state);
            }
        })?;

        // Handle keyboard events
        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key_event) = event::read()? {
                use crossterm::event::KeyEventKind;
                // Only process key press events, not releases or repeats
                if key_event.kind == KeyEventKind::Press {
                    let key = key_event;
                    if help_screen_visible {
                        // Handle help screen keys
                        match key.code {
                            KeyCode::Char('q') => break,
                            KeyCode::Esc => {
                                help_screen_visible = false;
                            }
                            _ => {}
                        }
                    } else if scroll_mode {
                        // Handle scroll mode keys
                        match key.code {
                            KeyCode::Char('q') => break,
                            KeyCode::Char('h') => {
                                scroll_mode = false;
                            }
                            KeyCode::Esc => {
                                // Exit scrollmode and collapse open accordion
                                if let Some(selected) = list_state.selected() {
                                    if selected > 0 {
                                        let actual_index = selected - 1;
                                        let grouped_messages = GroupedLogMessages::from_messages(&log_lines, &pinned_groups);
                                        let flat_items = create_flat_navigation_structure(&grouped_messages, &expanded_groups, &filter_text);
                                        
                                        if actual_index < flat_items.len() {
                                            if let FlatNavigationItem::Message(message) = &flat_items[actual_index] {
                                                if let Some(uid) = &message.uid {
                                                    expanded_uids.remove(uid);
                                                }
                                            }
                                        }
                                    }
                                }
                                scroll_mode = false;
                            }
                            KeyCode::Char('j') | KeyCode::Down => {
                                if let Some(selected) = list_state.selected() {
                                    if selected > 0 {
                                        let actual_index = selected - 1;
                                        let grouped_messages = GroupedLogMessages::from_messages(&log_lines, &pinned_groups);
                                        let flat_items = create_flat_navigation_structure(&grouped_messages, &expanded_groups, &filter_text);
                                        
                                        if actual_index < flat_items.len() {
                                            if let FlatNavigationItem::Message(message) = &flat_items[actual_index] {
                                                // Calculate actual content lines for this message
                                                let mut total_lines = 0;

                                                if message.statement.contains("[-- Batch Command") {
                                                    // Count batch processing lines
                                                    let mut current_batch_sql = String::new();
                                                    for statement_line in message.statement.lines() {
                                                        if statement_line.starts_with("[-- Batch Command") {
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
                                                                    total_lines += current_batch_sql.lines().count().max(1);
                                                                } else {
                                                                    total_lines += formatted_sql.lines().count();
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
                                                            total_lines += current_batch_sql.lines().count().max(1);
                                                        } else {
                                                            total_lines += formatted_sql.lines().count();
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
                                                    let formatted_sql = format(&message.statement, &QueryParams::None, &format_options);
                                                    if formatted_sql.trim().is_empty() {
                                                        total_lines += message.statement.lines().count().max(1);
                                                    } else {
                                                        total_lines += formatted_sql.lines().count();
                                                    }
                                                    total_lines += 1; // end statement marker
                                                }

                                                let current_cursor = scroll_cursors.get(&actual_index).cloned().unwrap_or(0);
                                                let current_offset = scroll_offsets.get(&actual_index).cloned().unwrap_or(0);

                                                // Move cursor down if not at the end
                                                if current_cursor < total_lines.saturating_sub(1) {
                                                    let new_cursor = current_cursor + 1;
                                                    scroll_cursors.insert(actual_index, new_cursor);

                                                    // Calculate dynamic expanded height
                                                    let min_reserved_space = 5;
                                                    let available_height = last_list_height.saturating_sub(4);
                                                    let dynamic_max_expanded_height = available_height.saturating_sub(min_reserved_space).max(10);

                                                    // Auto-scroll if cursor goes beyond visible area
                                                    if new_cursor >= current_offset + dynamic_max_expanded_height {
                                                        scroll_offsets.insert(actual_index, current_offset + 1);
                                                    }
                                                }
                                            }
                                        }
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
                                                                let grouped_messages = GroupedLogMessages::from_messages(&log_lines, &pinned_groups);
                                        let flat_items = create_flat_navigation_structure(&grouped_messages, &expanded_groups, &filter_text);
                                        
                                        if actual_index < flat_items.len() {
                                            if let FlatNavigationItem::Message(message) = &flat_items[actual_index] {
                                                // Page down (Ctrl+d) - move cursor down by half a page
                                                let current_cursor =
                                                    scroll_cursors.get(&actual_index).cloned().unwrap_or(0);
                                                let current_offset =
                                                    scroll_offsets.get(&actual_index).cloned().unwrap_or(0);
                                                // Calculate dynamic expanded height
                                                let min_reserved_space = 5;
                                                let available_height = last_list_height.saturating_sub(4);
                                                let dynamic_max_expanded_height = available_height.saturating_sub(min_reserved_space).max(10);
                                                let page_size = dynamic_max_expanded_height / 2; // Half page like vim

                                                // Calculate total lines for this message
                                                let mut total_lines = 0;

                                                if message.statement.contains("[-- Batch Command") {
                                                    // Count batch processing lines
                                                    let mut current_batch_sql = String::new();
                                                    for statement_line in message.statement.lines() {
                                                        if statement_line.starts_with("[-- Batch Command") {
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
                                                                    total_lines += current_batch_sql.lines().count().max(1);
                                                                } else {
                                                                    total_lines += formatted_sql.lines().count();
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
                                                            total_lines += current_batch_sql.lines().count().max(1);
                                                        } else {
                                                            total_lines += formatted_sql.lines().count();
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
                                                    let formatted_sql = format(&message.statement, &QueryParams::None, &format_options);
                                                    if formatted_sql.trim().is_empty() {
                                                        total_lines += message.statement.lines().count().max(1);
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
                                                if new_cursor >= current_offset + dynamic_max_expanded_height {
                                                    let new_offset = std::cmp::min(
                                                        current_offset + page_size,
                                                        total_lines.saturating_sub(dynamic_max_expanded_height),
                                                    );
                                                    scroll_offsets.insert(actual_index, new_offset);
                                                }
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
                                        // Calculate dynamic expanded height
                                        let min_reserved_space = 5;
                                        let available_height = last_list_height.saturating_sub(4);
                                        let dynamic_max_expanded_height = available_height.saturating_sub(min_reserved_space).max(10);
                                        let page_size = dynamic_max_expanded_height / 2; // Half page like vim

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
                                        let grouped_messages = GroupedLogMessages::from_messages(&log_lines, &pinned_groups);
                                        let flat_items = create_flat_navigation_structure(&grouped_messages, &expanded_groups, &filter_text);
                                        
                                        if actual_index < flat_items.len() {
                                            if let FlatNavigationItem::Message(message) = &flat_items[actual_index] {
                                                let cursor_pos = scroll_cursors
                                                    .get(&actual_index)
                                                    .cloned()
                                                    .unwrap_or(0);

                                                let text_to_copy =
                                                    if message.statement.contains("[-- Batch Command") {
                                                        format::extract_batch_statement_at_cursor(
                                                            &message.statement,
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
                                                            &message.statement,
                                                            &QueryParams::None,
                                                            &format_options,
                                                        );
                                                        if formatted_sql.trim().is_empty() {
                                                            message.statement.clone()
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
                                let grouped_messages = GroupedLogMessages::from_messages(&log_lines, &pinned_groups);
                                let total_items = count_total_rendered_items(&grouped_messages, &expanded_groups, &filter_text);
                                if let Some(selected) = list_state.selected() {
                                    if selected < total_items {
                                        // Account for padding line
                                        list_state.select(Some(selected + 1));
                                    }
                                } else if total_items > 0 {
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
                                        let actual_index = selected - 1; // Convert to actual navigation index
                                        let grouped_messages = GroupedLogMessages::from_messages(&log_lines, &pinned_groups);
                                        let flat_items = create_flat_navigation_structure(&grouped_messages, &expanded_groups, &filter_text);
                                        
                                        if actual_index < flat_items.len() {
                                            match &flat_items[actual_index] {
                                                FlatNavigationItem::GroupHeader(group) => {
                                                    // Toggle group expansion
                                                    if expanded_groups.contains(group) {
                                                        expanded_groups.remove(group);
                                                    } else {
                                                        expanded_groups.insert(group.clone());
                                                    }
                                                }
                                                FlatNavigationItem::Message(message) => {
                                                    // Toggle individual message expansion
                                                    if let Some(uid) = &message.uid {
                                                        if expanded_uids.contains(uid) {
                                                            expanded_uids.remove(uid);
                                                        } else {
                                                            expanded_uids.insert(uid.clone());
                                                        }
                                                    }
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
                                        let grouped_messages = GroupedLogMessages::from_messages(&log_lines, &pinned_groups);
                                        let flat_items = create_flat_navigation_structure(&grouped_messages, &expanded_groups, &filter_text);
                                        
                                        if actual_index < flat_items.len() {
                                            if let FlatNavigationItem::Message(message) = &flat_items[actual_index] {
                                                if let Some(uid) = &message.uid {
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
                            }
                            KeyCode::Char('h') => {
                                help_screen_visible = true;
                            }
                            KeyCode::Char('t') => {
                                if let Some(selected) = list_state.selected() {
                                    if selected > 0 {
                                        let actual_index = selected - 1;
                                        let grouped_messages = GroupedLogMessages::from_messages(&log_lines, &pinned_groups);
                                        let flat_items = create_flat_navigation_structure(&grouped_messages, &expanded_groups, &filter_text);

                                        if actual_index < flat_items.len() {
                                            if let FlatNavigationItem::GroupHeader(group) = &flat_items[actual_index] {
                                                // Capture the group we're toggling
                                                let target_group = group.clone();

                                                // Toggle pin status for the selected group
                                                if pinned_groups.contains(group) {
                                                    pinned_groups.remove(group);
                                                } else {
                                                    pinned_groups.insert(group.clone());
                                                }

                                                // After toggling, find where this group ended up and restore selection
                                                let updated_grouped_messages = GroupedLogMessages::from_messages(&log_lines, &pinned_groups);
                                                let updated_flat_items = create_flat_navigation_structure(&updated_grouped_messages, &expanded_groups, &filter_text);

                                                // Find the new position of the target group
                                                for (new_index, item) in updated_flat_items.iter().enumerate() {
                                                    if let FlatNavigationItem::GroupHeader(updated_group) = item {
                                                        if *updated_group == target_group {
                                                            list_state.select(Some(new_index + 1)); // +1 for padding line
                                                            break;
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            KeyCode::Char('c') => {
                                // Clear all log entries for a clean slate
                                log_lines.clear();
                                expanded_uids.clear();
                                expanded_groups.clear();
                                scroll_offsets.clear();
                                scroll_cursors.clear();
                                selected_uid = None;
                                list_state.select(Some(1)); // Reset selection to first position
                                main_scroll_offset = 0;
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

// Represents a flattened navigation item (either a group header or individual message)
#[derive(Clone, Debug)]
enum FlatNavigationItem<'a> {
    GroupHeader(RequestGroup),
    Message(&'a SqlLogMessage),
}

// Create a flattened navigation structure for the grouped messages
fn create_flat_navigation_structure<'a>(
    grouped_messages: &'a GroupedLogMessages,
    expanded_groups: &std::collections::HashSet<RequestGroup>,
    filter_text: &str,
) -> Vec<FlatNavigationItem<'a>> {
    let mut flat_items = Vec::new();
    
    for (group, messages) in &grouped_messages.groups {
        // Filter messages within the group
        let filtered_messages: Vec<&SqlLogMessage> = if filter_text.is_empty() {
            messages.iter().collect()
        } else {
            messages.iter().filter(|msg| {
                let method_match = if msg.http_method.is_none() {
                    "CALL".to_lowercase().contains(&filter_text.to_lowercase())
                } else {
                    msg.http_method
                        .as_ref()
                        .map_or(false, |method| method.to_lowercase().contains(&filter_text.to_lowercase()))
                };

                let endpoint_match = msg
                    .endpoint
                    .as_ref()
                    .map_or(false, |endpoint| endpoint.to_lowercase().contains(&filter_text.to_lowercase()));

                let caller_class_match = msg
                    .caller_class
                    .as_ref()
                    .map_or(false, |class| class.to_lowercase().contains(&filter_text.to_lowercase()));

                let caller_method_match = msg
                    .caller_method
                    .as_ref()
                    .map_or(false, |method| method.to_lowercase().contains(&filter_text.to_lowercase()));

                method_match || endpoint_match || caller_class_match || caller_method_match
            }).collect()
        };
        
        // Skip groups with no matching messages
        if filtered_messages.is_empty() {
            continue;
        }
        
        // Add group header
        flat_items.push(FlatNavigationItem::GroupHeader(group.clone()));
        
        // If group is expanded, add individual messages
        if expanded_groups.contains(group) {
            // Sort messages by timestamp (most recent first)
            let mut sorted_messages = filtered_messages;
            sorted_messages.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
            
            for message in sorted_messages {
                flat_items.push(FlatNavigationItem::Message(message));
            }
        }
    }
    
    flat_items
}

// Count total rendered items in grouped structure for navigation
fn count_total_rendered_items(
    grouped_messages: &GroupedLogMessages,
    expanded_groups: &std::collections::HashSet<RequestGroup>,
    filter_text: &str,
) -> usize {
    create_flat_navigation_structure(grouped_messages, expanded_groups, filter_text).len()
}
