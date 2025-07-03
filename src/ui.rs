use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
    widgets::ListState,
};

use sqlformat::{FormatOptions, QueryParams, format};

use crate::{
    MAX_EXPANDED_HEIGHT, SqlLogMessage,
    format::{classify_sql_size, highlight_sql, sql_size_color},
};

// Helper function for rendering the header row
pub fn render_header_row(
    arrow: &str,
    formatted_duration: &str,
    sql_len: usize,
    sql_color: Color,
    http_method: &str,
    method_color: Color,
    endpoint_str: &str,
    is_flashing: bool,
    flash_bg: Color,
    flash_fg: Color,
    rgb: (u8, u8, u8),
    width: usize,
) -> Line<'static> {
    let (r, g, b) = rgb;
    let arrow_duration_text = format!(" {} {:7} ", arrow, formatted_duration);
    let char_count_text = format!(" {:>5} ", sql_len);
    let method_text = format!(" {} ", http_method);
    let endpoint_text = format!(" {} ", endpoint_str);

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
        method_text.clone(),
        if is_flashing {
            Style::default().bg(flash_bg).fg(flash_fg)
        } else {
            Style::default().bg(method_color).fg(Color::Black)
        },
    ));
    header_spans.push(Span::styled(
        endpoint_text.clone(),
        if is_flashing {
            Style::default().bg(flash_bg).fg(flash_fg)
        } else {
            Style::default().bg(Color::Rgb(40, 40, 40)).fg(Color::White)
        },
    ));
    let used_width =
        arrow_duration_text.len() + char_count_text.len() + method_text.len() + endpoint_text.len();
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

/// Render a single accordion item for the SQL log list.
#[allow(clippy::too_many_arguments)]
pub fn render_accordion_item(
    index: usize,
    line: &crate::SqlLogMessage,
    expanded_items: &std::collections::HashSet<usize>,
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
    let style = if is_flashing {
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
    let is_expanded = expanded_items.contains(&index);
    let arrow = if is_expanded { "▼" } else { "►" };
    let mut lines = vec![];
    if is_expanded {
        let (endpoint_str, http_method) = if line.http_method.is_none() {
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
        let header_line = render_header_row(
            arrow,
            &formatted_duration,
            sql_len,
            sql_color,
            &http_method,
            crate::get_http_method_color(&http_method),
            &endpoint_str,
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
        let scroll_offset = scroll_offsets.get(&index).cloned().unwrap_or(0);
        let total_content_lines = all_content_lines.len();
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
        let end_offset = std::cmp::min(scroll_offset + max_expanded_height, total_content_lines);
        let cursor_pos = scroll_cursors.get(&index).cloned().unwrap_or(0);
        for (content_index, content_line) in all_content_lines
            .iter()
            .skip(scroll_offset)
            .take(max_expanded_height)
            .enumerate()
        {
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
                lines.push(content_line.clone());
            }
        }
        if total_content_lines > max_expanded_height {
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
        // Removed bottom padding line
    } else {
        let (endpoint_str, http_method) = if line.http_method.is_none() {
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
        let header_line = render_header_row(
            arrow,
            &formatted_duration,
            sql_len,
            sql_color,
            &http_method,
            crate::get_http_method_color(&http_method),
            &endpoint_str,
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
        let original_lines: Vec<&str> = if sql.trim().is_empty() {
            vec!["No Statement - Original Empty"]
        } else {
            sql.lines().collect()
        };
        let display_lines: Vec<&str> = if original_lines.is_empty()
            || original_lines.iter().all(|line| line.trim().is_empty())
        {
            vec!["No Statement - All Lines Empty"]
        } else {
            original_lines
        };
        let sql_to_highlight =
            if display_lines.len() == 1 && display_lines[0].contains("No Statement") {
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

pub fn handle_down(
    log_lines: &Vec<SqlLogMessage>,
    list_state: &ListState,
    scroll_offsets: &mut std::collections::HashMap<usize, usize>,
    scroll_cursors: &mut std::collections::HashMap<usize, usize>,
) {
    if let Some(selected) = list_state.selected() {
        if selected > 0 {
            let actual_index = selected - 1;
            // We need to recalculate the actual content lines for accurate scrolling
            if actual_index < log_lines.len() {
                let line = &log_lines[log_lines.len() - 1 - actual_index];

                // Calculate actual content lines (same logic as in rendering)
                let mut actual_content_lines = 0;

                if line.statement.contains("[-- Batch Command") {
                    // Count batch processing lines
                    let mut current_batch_sql = String::new();
                    for statement_line in line.statement.lines() {
                        if statement_line.starts_with("[-- Batch Command") {
                            if !current_batch_sql.trim().is_empty() {
                                actual_content_lines += 1; // batch header
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
                                    actual_content_lines +=
                                        current_batch_sql.lines().count().max(1);
                                } else {
                                    actual_content_lines += formatted_sql.lines().count();
                                }
                                actual_content_lines += 1; // separator
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
                        actual_content_lines += 1; // batch header
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
                            actual_content_lines += current_batch_sql.lines().count().max(1);
                        } else {
                            actual_content_lines += formatted_sql.lines().count();
                        }
                    }
                } else {
                    // Regular statement
                    actual_content_lines += 1; // mode indicator
                    let format_options = FormatOptions {
                        indent: sqlformat::Indent::Spaces(2),
                        uppercase: Some(false),
                        lines_between_queries: 1,
                        ignore_case_convert: Some(vec![]),
                    };
                    let formatted_sql =
                        format(&line.statement, &QueryParams::None, &format_options);
                    if formatted_sql.trim().is_empty() {
                        actual_content_lines += line.statement.lines().count().max(1);
                    } else {
                        actual_content_lines += formatted_sql.lines().count();
                    }
                    actual_content_lines += 1; // end statement marker
                }

                let current_cursor = scroll_cursors.get(&actual_index).cloned().unwrap_or(0);
                let current_offset = scroll_offsets.get(&actual_index).cloned().unwrap_or(0);

                // Move cursor down if not at the end
                if current_cursor < actual_content_lines.saturating_sub(1) {
                    let new_cursor = current_cursor + 1;
                    scroll_cursors.insert(actual_index, new_cursor);

                    // Auto-scroll if cursor goes beyond visible area
                    if new_cursor >= current_offset + MAX_EXPANDED_HEIGHT {
                        scroll_offsets.insert(actual_index, current_offset + 1);
                    }
                }
            }
        }
    }
}
