use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};
use chrono::{DateTime, Local, Utc};

use crate::{
    SqlLogMessage, RequestGroup, GroupedLogMessages,
    format::{classify_sql_size, highlight_sql, sql_size_color},
};

// Helper function to extract HH:MM:SS from timestamp and convert to local time
fn extract_time_from_timestamp(timestamp: &str) -> String {
    // Try to parse as RFC3339/ISO 8601 format and convert to local time
    if let Ok(dt) = DateTime::parse_from_rfc3339(timestamp) {
        let local_time: DateTime<Local> = dt.with_timezone(&Local);
        return local_time.format("%H:%M:%S").to_string();
    }
    
    // Try to parse as UTC timestamp without timezone info
    if let Ok(dt) = timestamp.parse::<DateTime<Utc>>() {
        let local_time: DateTime<Local> = dt.with_timezone(&Local);
        return local_time.format("%H:%M:%S").to_string();
    }
    
    // Try to extract time portion from various timestamp formats as fallback
    if let Some(time_part) = timestamp.split('T').nth(1) {
        // ISO format like "2023-12-01T14:30:25.123Z"
        if let Some(time_only) = time_part.split('.').next() {
            return time_only.to_string();
        }
    }
    
    // If timestamp contains space, try splitting on space
    if let Some(time_part) = timestamp.split(' ').nth(1) {
        if let Some(time_only) = time_part.split('.').next() {
            return time_only.to_string();
        }
    }
    
    // Fallback - just return the timestamp as is
    timestamp.to_string()
}

// Helper function for rendering the header row
pub fn render_header_row(
    arrow: &str,
    formatted_duration: &str,
    sql_len: usize,
    sql_color: Color,
    time_str: &str,
    is_flashing: bool,
    flash_bg: Color,
    flash_fg: Color,
    rgb: (u8, u8, u8),
    width: usize,
) -> Line<'static> {
    let (r, g, b) = rgb;
    let arrow_duration_text = format!(" {} {:7} ", arrow, formatted_duration);
    let char_count_text = format!(" {:>5} ", sql_len);
    let time_text = format!(" {} ", time_str);

    let mut header_spans = Vec::new();
    header_spans.push(Span::styled(
        arrow_duration_text.clone(),
        if is_flashing {
            Style::default().bg(flash_bg).fg(flash_fg)
        } else {
            Style::default().bg(Color::Rgb(r, g, b)).fg(Color::Black)
        },
    ));
    header_spans.push(Span::styled(
        char_count_text.clone(),
        if is_flashing {
            Style::default().bg(flash_bg).fg(flash_fg)
        } else {
            Style::default().bg(sql_color).fg(Color::Black)
        },
    ));
    header_spans.push(Span::styled(
        time_text.clone(),
        if is_flashing {
            Style::default().bg(flash_bg).fg(flash_fg)
        } else {
            Style::default().bg(Color::Rgb(100, 100, 100)).fg(Color::White)
        },
    ));
    let used_width = arrow_duration_text.len() + char_count_text.len() + time_text.len();
    if used_width < width {
        let remaining_space = " ".repeat(width - used_width);
        header_spans.push(Span::styled(
            remaining_space,
            if is_flashing {
                Style::default().bg(flash_bg).fg(flash_fg)
            } else {
                Style::default().bg(Color::Black)
            },
        ));
    }
    Line::from(header_spans)
}

// Render a group header for the grouped accordion
pub fn render_group_header(
    group: &RequestGroup,
    message_count: usize,
    is_expanded: bool,
    is_pinned: bool,
    width: usize,
) -> ratatui::widgets::ListItem<'static> {
    let arrow = if is_expanded { "▼" } else { "►" };
    let method_color = crate::get_http_method_color(&group.http_method);
    
    let _header_text = format!(
        " {} [{}] {} {}",
        arrow,
        message_count,
        group.http_method,
        group.endpoint
    );
    
    let mut spans = vec![
        Span::styled(
            format!(" {} ", arrow),
            Style::default().bg(Color::Rgb(60, 60, 60)).fg(Color::White)
        ),
        Span::styled(
            format!(" [{}] ", message_count),
            Style::default().bg(Color::Rgb(80, 80, 80)).fg(Color::Yellow)
        ),
        Span::styled(
            format!(" {} ", group.http_method),
            Style::default().bg(method_color).fg(Color::Black)
        ),
        Span::styled(
            format!(" {} ", group.endpoint),
            Style::default().bg(Color::Rgb(40, 40, 40)).fg(Color::White)
        ),
    ];
    
    // Add pin indicator if the group is pinned
    if is_pinned {
        spans.push(Span::styled(
            " 📌 ",
            Style::default().bg(Color::Rgb(255, 215, 0)).fg(Color::Black) // Gold background
        ));
    }
    
    let used_width: usize = spans.iter().map(|s| s.content.len()).sum();
    if used_width < width {
        let remaining_space = " ".repeat(width - used_width);
        spans.push(Span::styled(
            remaining_space,
            Style::default().bg(Color::Black),
        ));
    }
    
    let lines = vec![
        Line::from(spans),
        Line::from(Span::styled(
            "─".repeat(width),
            Style::default().fg(Color::Rgb(80, 80, 80)),
        )),
    ];
    
    ratatui::widgets::ListItem::new(lines)
}

// Render grouped accordions
pub fn render_grouped_accordions(
    grouped_messages: &GroupedLogMessages,
    expanded_groups: &std::collections::HashSet<RequestGroup>,
    expanded_uids: &std::collections::HashSet<String>,
    copy_flash_state: Option<(usize, std::time::Instant)>,
    list_state: &ratatui::widgets::ListState,
    scroll_mode: bool,
    scroll_offsets: &std::collections::HashMap<usize, usize>,
    scroll_cursors: &std::collections::HashMap<usize, usize>,
    max_expanded_height: usize,
    width: usize,
    filter_text: &str,
    pinned_groups: &std::collections::HashSet<RequestGroup>,
) -> Vec<ratatui::widgets::ListItem<'static>> {
    let mut items = Vec::new();
    let mut flat_index = 0; // Track flattened index for selection
    
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
        
        // Render group header
        let is_group_expanded = expanded_groups.contains(group);
        let is_pinned = pinned_groups.contains(group);
        let group_item = render_group_header(group, filtered_messages.len(), is_group_expanded, is_pinned, width);
        items.push(group_item);
        flat_index += 1;
        
        // If group is expanded, render individual messages
        if is_group_expanded {
            // Sort messages by timestamp (most recent first)
            let mut sorted_messages = filtered_messages;
            sorted_messages.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
            
            for (_msg_index, message) in sorted_messages.iter().enumerate() {
                let item = render_accordion_item(
                    flat_index,
                    message,
                    expanded_uids,
                    copy_flash_state,
                    list_state,
                    scroll_mode,
                    scroll_offsets,
                    scroll_cursors,
                    max_expanded_height,
                    width,
                );
                items.push(item);
                flat_index += 1;
            }
        }
    }
    
    items
}

/// Render a single accordion item for the SQL log list.
#[allow(clippy::too_many_arguments)]
pub fn render_accordion_item(
    index: usize,
    line: &crate::SqlLogMessage,
    expanded_uids: &std::collections::HashSet<String>,
    copy_flash_state: Option<(usize, std::time::Instant)>,
    list_state: &ratatui::widgets::ListState,
    scroll_mode: bool,
    scroll_offsets: &std::collections::HashMap<usize, usize>,
    scroll_cursors: &std::collections::HashMap<usize, usize>,
    max_expanded_height: usize,
    width: usize,
) -> ratatui::widgets::ListItem<'static> {
    use ratatui::{
        style::Style,
        text::{Line, Span},
    };
    let (r, g, b) = crate::interpolate_color(line.duration);
    let sql_len = line.statement.chars().count();
    let sql_class = classify_sql_size(sql_len);
    let sql_color = sql_size_color(sql_class);
    let is_flashing = if let Some((flash_index, _)) = copy_flash_state {
        flash_index == index
    } else {
        false
    };
    let _style = if is_flashing {
        Style::default()
            .bg(ratatui::style::Color::Rgb(0, 255, 0))
            .fg(ratatui::style::Color::Rgb(0, 0, 0))
    } else {
        Style::default()
            .bg(ratatui::style::Color::Rgb(r, g, b))
            .fg(ratatui::style::Color::Rgb(0, 0, 0))
    };
    let flash_bg = ratatui::style::Color::Rgb(0, 255, 0);
    let flash_fg = ratatui::style::Color::Rgb(0, 0, 0);
    let formatted_duration = crate::format_duration(line.duration);
    let is_expanded = match &line.uid {
        Some(uid) => expanded_uids.contains(uid),
        None => false,
    };
    let arrow = if is_expanded { "▼" } else { "►" };
    let mut lines = vec![];
    if is_expanded {
        let (_endpoint_str, _http_method) = if line.http_method.is_none() {
            // Show caller info when http_method is null
            let caller_info = match (&line.caller_method, &line.caller_class) {
                (Some(method), Some(class)) => format!("{} in {}", method, class),
                (Some(method), None) => method.clone(),
                (None, Some(class)) => format!("in {}", class),
                (None, None) => "N/A".to_string(),
            };
            (caller_info, "CALL".to_string())
        } else {
            (
                line.endpoint.clone().unwrap_or("N/A".to_string()),
                line.http_method.clone().unwrap_or("UNKNOWN".to_string()),
            )
        };
        let time_str = extract_time_from_timestamp(&line.timestamp);
        let header_line = render_header_row(
            arrow,
            &formatted_duration,
            sql_len,
            sql_color,
            &time_str,
            is_flashing,
            flash_bg,
            flash_fg,
            (r, g, b),
            width,
        );
        lines.push(header_line);
        let max_line_width = width.saturating_sub(4);
        let sql_bg_color = ratatui::style::Color::Black;
        let mut all_content_lines = Vec::new();
        if line.statement.contains("[-- Batch Command") {
            let mut current_batch_sql = String::new();
            let mut batch_number = 1;
            for statement_line in line.statement.lines() {
                if statement_line.starts_with("[-- Batch Command") {
                    if !current_batch_sql.trim().is_empty() {
                        let batch_header = format!("[-- Batch Command {}]", batch_number);
                        all_content_lines.push(Line::from(Span::styled(
                            format!("  {:<width$}  ", batch_header, width = max_line_width),
                            Style::default()
                                .bg(ratatui::style::Color::Rgb(30, 30, 30))
                                .fg(ratatui::style::Color::Yellow),
                        )));
                        all_content_lines.extend(render_sql_lines(
                            &current_batch_sql,
                            max_line_width,
                            sql_bg_color,
                        ));
                        all_content_lines.push(Line::from(Span::styled(
                            format!("  {:<width$}  ", "", width = max_line_width),
                            Style::default().bg(sql_bg_color),
                        )));
                        batch_number += 1;
                    }
                    current_batch_sql.clear();
                } else {
                    if !current_batch_sql.is_empty() {
                        current_batch_sql.push('\n');
                    }
                    current_batch_sql.push_str(statement_line);
                }
            }
            if !current_batch_sql.trim().is_empty() {
                let batch_header = format!("[-- Batch Command {}]", batch_number);
                all_content_lines.push(Line::from(Span::styled(
                    format!("  {:<width$}  ", batch_header, width = max_line_width),
                    Style::default()
                        .bg(ratatui::style::Color::Rgb(40, 40, 40))
                        .fg(ratatui::style::Color::Yellow),
                )));
                all_content_lines.extend(render_sql_lines(
                    &current_batch_sql,
                    max_line_width,
                    sql_bg_color,
                ));
            }
        } else {
            all_content_lines.extend(render_sql_lines(
                &line.statement,
                max_line_width,
                sql_bg_color,
            ));
            all_content_lines.push(Line::from(Span::styled(
                format!(
                    "  {:<width$}  ",
                    "=== END STATEMENT ===",
                    width = max_line_width
                ),
                Style::default()
                    .bg(ratatui::style::Color::Rgb(50, 50, 50))
                    .fg(ratatui::style::Color::White),
            )));
        }
        // Clamp scroll_offset to valid range to prevent blank screens
        let total_content_lines = all_content_lines.len();
        let max_scroll_offset = total_content_lines.saturating_sub(max_expanded_height);
        // Always clamp scroll_offset to valid range on every render (handles terminal resize)
        let mut scroll_offset = scroll_offsets.get(&index).cloned().unwrap_or(0);
        // If content fits, always show from top
        if total_content_lines <= max_expanded_height {
            scroll_offset = 0;
        } else {
            // Clamp scroll_offset to last possible valid starting index
            if scroll_offset > max_scroll_offset {
                scroll_offset = max_scroll_offset;
            }
        }
        // Defensive: always show at least one line if content exists
        if total_content_lines == 0 {
            lines.push(Line::from(Span::styled(
                "  (no content to display)  ",
                Style::default().bg(sql_bg_color).fg(ratatui::style::Color::Red),
            )));
        } else {
            // Always show last max_expanded_height lines if scroll_offset is out of bounds
            let mut visible_lines: Vec<_> = all_content_lines
                .iter()
                .skip(scroll_offset)
                .take(max_expanded_height)
                .collect();
            if visible_lines.is_empty() && total_content_lines > 0 {
                let fallback_offset = total_content_lines.saturating_sub(max_expanded_height);
                visible_lines = all_content_lines
                    .iter()
                    .skip(fallback_offset)
                    .take(max_expanded_height)
                    .collect();
            }
            if visible_lines.is_empty() {
                lines.push(Line::from(Span::styled(
                    "  (content not visible, check scroll position)  ",
                    Style::default().bg(sql_bg_color).fg(ratatui::style::Color::Yellow),
                )));
            } else {
                // Add scroll info if needed
                if total_content_lines > max_expanded_height {
                    let scroll_info = if scroll_mode && list_state.selected() == Some(index + 1) {
                        format!(
                            "SCROLL MODE: Line {}/{} (j/k to scroll, h to exit)",
                            scroll_offset + 1,
                            total_content_lines
                        )
                    } else {
                        format!(
                            "Content too long: {} lines (press 'l' to scroll)",
                            total_content_lines
                        )
                    };
                    lines.push(Line::from(Span::styled(
                        format!("  {:<width$}  ", scroll_info, width = max_line_width),
                        Style::default()
                            .bg(ratatui::style::Color::Rgb(50, 100, 150))
                            .fg(ratatui::style::Color::White),
                    )));
                }
                let cursor_pos = scroll_cursors.get(&index).cloned().unwrap_or(0);
                for (content_index, content_line) in visible_lines.iter().enumerate() {
                    let absolute_line_index = scroll_offset + content_index;
                    if scroll_mode
                        && list_state.selected() == Some(index + 1)
                        && absolute_line_index == cursor_pos
                    {
                        let cursor_line = match content_line {
                            Line { spans, .. } => {
                                let mut new_spans = Vec::new();
                                for span in spans {
                                    new_spans.push(Span::styled(
                                        span.content.clone(),
                                        span.style
                                            .bg(ratatui::style::Color::Blue)
                                            .fg(ratatui::style::Color::Yellow),
                                    ));
                                }
                                Line::from(new_spans)
                            }
                        };
                        lines.push(cursor_line);
                    } else {
                        lines.push((*content_line).clone());
                    }
                }
                // Add bottom info if needed
                if total_content_lines > max_expanded_height {
                    let end_offset = std::cmp::min(scroll_offset + max_expanded_height, total_content_lines);
                    let remaining = total_content_lines.saturating_sub(end_offset);
                    if remaining > 0 {
                        lines.push(Line::from(Span::styled(
                            format!(
                                "  {:<width$}  ",
                                format!("... {} more lines below", remaining),
                                width = max_line_width
                            ),
                            Style::default()
                                .bg(ratatui::style::Color::Rgb(100, 100, 100))
                                .fg(ratatui::style::Color::White),
                        )));
                    }
                }
            }
        }
        // Removed bottom padding line
    } else {
        let (_endpoint_str, _http_method) = if line.http_method.is_none() {
            // Show caller info when http_method is null
            let caller_info = match (&line.caller_method, &line.caller_class) {
                (Some(method), Some(class)) => format!("{} in {}", method, class),
                (Some(method), None) => method.clone(),
                (None, Some(class)) => format!("in {}", class),
                (None, None) => "N/A".to_string(),
            };
            (caller_info, "CALL".to_string())
        } else {
            (
                line.endpoint.clone().unwrap_or("N/A".to_string()),
                line.http_method.clone().unwrap_or("UNKNOWN".to_string()),
            )
        };
        let time_str = extract_time_from_timestamp(&line.timestamp);
        let header_line = render_header_row(
            arrow,
            &formatted_duration,
            sql_len,
            sql_color,
            &time_str,
            is_flashing,
            flash_bg,
            flash_fg,
            (r, g, b),
            width,
        );
        lines.push(header_line);
    }
    lines.push(Line::from(Span::styled(
        "─".repeat(width),
        Style::default().fg(ratatui::style::Color::Black),
    )));
    ratatui::widgets::ListItem::new(lines)
}

/// Render SQL lines with syntax highlighting and padding.
pub fn render_sql_lines(
    sql: &str,
    max_line_width: usize,
    sql_bg_color: Color,
) -> Vec<Line<'static>> {
    let format_options = sqlformat::FormatOptions {
        indent: sqlformat::Indent::Spaces(2),
        uppercase: Some(false),
        lines_between_queries: 1,
        ignore_case_convert: Some(vec![]),
    };
    let formatted_sql = sqlformat::format(sql, &sqlformat::QueryParams::None, &format_options);
    let formatted_lines: Vec<&str> = formatted_sql.lines().collect();
    let mut lines = Vec::new();
    if formatted_lines.is_empty() || formatted_sql.trim().is_empty() {
        // Always display at least one line for SQL, even if empty or whitespace
        let original_lines: Vec<&str> = if sql.trim().is_empty() {
            vec!["(empty statement)"]
        } else {
            sql.lines().collect()
        };
        let display_lines: Vec<&str> = if original_lines.is_empty()
            || original_lines.iter().all(|line| line.trim().is_empty())
        {
            vec!["(empty statement)"]
        } else {
            original_lines
        };
        let sql_to_highlight =
            if display_lines.len() == 1 && display_lines[0].contains("(empty statement)") {
                display_lines[0].to_string()
            } else {
                sql.to_string()
            };
        let highlighted_text = highlight_sql(sql_to_highlight);
        for highlighted_line in highlighted_text.lines {
            let content_len: usize = highlighted_line.spans.iter().map(|s| s.content.len()).sum();
            let mut padded_spans = vec![Span::styled("  ", Style::default().bg(sql_bg_color))];
            for span in highlighted_line.spans {
                padded_spans.push(Span::styled(span.content, span.style.bg(sql_bg_color)));
            }
            let remaining_width = max_line_width.saturating_sub(content_len);
            if remaining_width > 0 {
                padded_spans.push(Span::styled(
                    " ".repeat(remaining_width),
                    Style::default().bg(sql_bg_color),
                ));
            }
            padded_spans.push(Span::styled("  ", Style::default().bg(sql_bg_color)));
            lines.push(Line::from(padded_spans));
        }
    } else {
        let highlighted_text = highlight_sql(formatted_sql.clone());
        for highlighted_line in highlighted_text.lines {
            let content_len: usize = highlighted_line.spans.iter().map(|s| s.content.len()).sum();
            let mut padded_spans = vec![Span::styled("  ", Style::default().bg(sql_bg_color))];
            for span in highlighted_line.spans {
                padded_spans.push(Span::styled(span.content, span.style.bg(sql_bg_color)));
            }
            let remaining_width = max_line_width.saturating_sub(content_len);
            if remaining_width > 0 {
                padded_spans.push(Span::styled(
                    " ".repeat(remaining_width),
                    Style::default().bg(sql_bg_color),
                ));
            }
            padded_spans.push(Span::styled("  ", Style::default().bg(sql_bg_color)));
            lines.push(Line::from(padded_spans));
        }
    }
    lines
}

// This function is deprecated and replaced by inline scroll handling in main.rs
// The grouped accordion structure makes this centralized function obsolete
// All scroll handling is now done directly in the scroll mode handlers in main.rs
