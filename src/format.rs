use ratatui::{
    style::{Color, Style},
    text::{Line, Span, Text},
};
use sqlformat::{FormatOptions, QueryParams, format as sql_format};
use syntect::{
    easy::HighlightLines,
    highlighting::{Style as SynStyle, ThemeSet},
    parsing::SyntaxSet,
    util::LinesWithEndings,
};


#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqlSizeClass {
    Small,
    Medium,
    Big,
    Abomination,
}

/// Convert a syntect style to a ratatui style, with special handling for greys.
pub fn syn_style_to_ratatui(span: SynStyle) -> Style {
    let (r, g, b) = (span.foreground.r, span.foreground.g, span.foreground.b);

    // Check if the color is grey-ish and convert to beige
    let is_grey = r == g && g == b && r > 100 && r < 180; // Grey tones between 100-180
    let is_dark_grey =
        (r as i32 - g as i32).abs() < 20 && (g as i32 - b as i32).abs() < 20 && r > 80 && r < 140; // Allow slight variations in grey

    if is_grey || is_dark_grey {
        // Convert to beige: warm, light brown color
        Style::default().fg(Color::Rgb(245, 222, 179)) // Wheat/beige color
    } else {
        Style::default().fg(Color::Rgb(r, g, b))
    }
}

/// Highlight SQL using syntect and convert to ratatui Text
pub fn highlight_sql(sql: String) -> Text<'static> {
    let ps = SyntaxSet::load_defaults_newlines();
    let ts = ThemeSet::load_defaults();
    let syntax = ps.find_syntax_by_extension("sql").unwrap();

    // Try a different theme - "base16-ocean.dark" tends to have better color contrast
    let theme_name = if ts.themes.contains_key("base16-ocean.dark") {
        "base16-ocean.dark"
    } else if ts.themes.contains_key("Solarized (dark)") {
        "Solarized (dark)"
    } else if ts.themes.contains_key("Monokai") {
        "Monokai"
    } else {
        "InspiredGitHub" // fallback
    };

    let mut h = HighlightLines::new(syntax, &ts.themes[theme_name]);

    let mut lines = Vec::new();

    for line in LinesWithEndings::from(&sql) {
        let ranges: Vec<(SynStyle, &str)> = h.highlight_line(line, &ps).unwrap();
        let mut spans = Vec::new();

        for (style, part) in ranges {
            spans.push(Span::styled(part.to_string(), syn_style_to_ratatui(style)));
        }

        lines.push(Line::from(spans));
    }

    Text::from(lines)
}

pub fn extract_batch_statement_at_cursor(statement: &str, cursor_pos: usize) -> String {
    // Parse batch statements and find which one the cursor is positioned on
    let mut current_batch_sql = String::new();
    let mut batch_statements = Vec::new();
    let mut line_count = 0;

    for statement_line in statement.lines() {
        if statement_line.starts_with("[-- Batch Command") {
            // Save previous batch if it exists
            if !current_batch_sql.trim().is_empty() {
                batch_statements.push((line_count, current_batch_sql.clone()));
                // Count lines for this batch (header + formatted lines + separator)
                let format_options = FormatOptions {
                    indent: sqlformat::Indent::Spaces(2),
                    uppercase: Some(false),
                    lines_between_queries: 1,
                    ignore_case_convert: Some(vec![]),
                };
                let formatted_sql = sql_format(
                    &current_batch_sql.trim(),
                    &QueryParams::None,
                    &format_options,
                );
                line_count += 1; // batch header
                if formatted_sql.trim().is_empty() {
                    line_count += current_batch_sql.lines().count().max(1);
                } else {
                    line_count += formatted_sql.lines().count();
                }
                line_count += 1; // separator
            }
            current_batch_sql.clear();
        } else {
            // Add line to current batch
            if !current_batch_sql.is_empty() {
                current_batch_sql.push('\n');
            }
            current_batch_sql.push_str(statement_line);
        }
    }

    // Handle final batch
    if !current_batch_sql.trim().is_empty() {
        batch_statements.push((line_count, current_batch_sql.clone()));
    }

    // Find which batch the cursor is in
    for (start_line, batch_sql) in batch_statements.iter() {
        let format_options = FormatOptions {
            indent: sqlformat::Indent::Spaces(2),
            uppercase: Some(false),
            lines_between_queries: 1,
            ignore_case_convert: Some(vec![]),
        };
        let formatted_sql = sql_format(&batch_sql.trim(), &QueryParams::None, &format_options);

        let batch_line_count = if formatted_sql.trim().is_empty() {
            batch_sql.lines().count().max(1)
        } else {
            formatted_sql.lines().count()
        };

        let end_line = start_line + 1 + batch_line_count + 1; // header + content + separator

        if cursor_pos >= *start_line && cursor_pos < end_line {
            // Return the formatted SQL if available, otherwise original
            if formatted_sql.trim().is_empty() {
                return batch_sql.clone();
            } else {
                return formatted_sql;
            }
        }
    }

    // Fallback: return the full statement if we can't determine which batch
    statement.to_string()
}

pub fn classify_sql_size(len: usize) -> SqlSizeClass {
    match len {
        0..=1249 => SqlSizeClass::Small,
        1250..=2499 => SqlSizeClass::Medium,
        2500..=4999 => SqlSizeClass::Big,
        _ => SqlSizeClass::Abomination,
    }
}

pub fn sql_size_color(class: SqlSizeClass) -> Color {
    match class {
        SqlSizeClass::Small => Color::Rgb(80, 200, 120), // Green
        SqlSizeClass::Medium => Color::Rgb(255, 193, 7), // Amber/Yellow
        SqlSizeClass::Big => Color::Rgb(255, 87, 34),    // Deep Orange
        SqlSizeClass::Abomination => Color::Rgb(186, 48, 255), // Vivid Purple
    }
}
