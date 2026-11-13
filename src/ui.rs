// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::OnceLock;

use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::App;
use crate::buffer::{
    contains_nick_mention, strip_user_prefix, Buffer, BufferKind, ChatLine, LineKind,
};
use crate::command::COMMAND_SPECS;

const NICK_PALETTE: &[Color] = &[
    Color::Cyan,
    Color::Green,
    Color::Yellow,
    Color::Magenta,
    Color::LightBlue,
    Color::LightRed,
    Color::LightCyan,
    Color::LightGreen,
    Color::LightYellow,
    Color::LightMagenta,
    Color::Blue,
    Color::Red,
];

pub fn nick_color(nick: &str) -> Color {
    let (_, clean) = strip_user_prefix(nick);
    let mut hash: u32 = 5381;
    for b in clean.bytes() {
        hash = hash.wrapping_mul(33).wrapping_add(b.to_ascii_lowercase() as u32);
    }
    NICK_PALETTE[(hash as usize) % NICK_PALETTE.len()]
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeRow<'a> {
    Server {
        name: &'a str,
        active: bool,
        unread: bool,
    },
    Child {
        name: &'a str,
        active: bool,
        unread: bool,
        highlight: bool,
    },
    Separator,
}

pub fn build_tree_rows(app: &App) -> Vec<TreeRow<'_>> {
    let mut rows = Vec::with_capacity(app.buffers.len() + app.servers.len());

    for (i, buf) in app.buffers.iter().enumerate() {
        let active = i == app.active_buffer;
        if buf.kind == BufferKind::Server {
            if !rows.is_empty() {
                rows.push(TreeRow::Separator);
            }
            rows.push(TreeRow::Server {
                name: &buf.name,
                active,
                unread: buf.unread,
            });
        } else {
            rows.push(TreeRow::Child {
                name: &buf.name,
                active,
                unread: buf.unread,
                highlight: buf.highlight,
            });
        }
    }

    rows
}

pub fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(24),
            Constraint::Min(1),
            Constraint::Length(20),
        ])
        .split(area);

    let center_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(columns[1]);

    draw_tree_sidebar(f, app, columns[0]);
    draw_chat_pane(f, app, center_chunks[0]);
    f.render_widget(Block::default().borders(Borders::TOP), center_chunks[1]);
    draw_input_bar(f, app, center_chunks[2]);
    draw_users_sidebar(f, app, columns[2]);

    if app.show_help {
        draw_help_popup(f, app);
    }
}

fn draw_tree_sidebar(f: &mut Frame, app: &App, area: Rect) {
    let tree_rows = build_tree_rows(app);
    let mut items = Vec::with_capacity(tree_rows.len());
    let mut active_row_idx = 0usize;

    for (idx, row) in tree_rows.into_iter().enumerate() {
        match row {
            TreeRow::Separator => {
                items.push(ListItem::new(Line::from("")));
            }
            TreeRow::Server {
                name,
                active,
                unread,
            } => {
                if active {
                    active_row_idx = idx;
                }
                let prefix = if active { "> " } else { "  " };
                let style = if active {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else if unread {
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().add_modifier(Modifier::BOLD)
                };
                items.push(ListItem::new(Line::from(Span::styled(
                    format!("{prefix}{name}"),
                    style,
                ))));
            }
            TreeRow::Child {
                name,
                active,
                unread,
                highlight,
            } => {
                if active {
                    active_row_idx = idx;
                }
                let conn = if active { ">   └ " } else { "    └ " };
                let conn_style = if active {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Gray)
                };
                let name_style = if active {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else if highlight {
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                } else if unread {
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                items.push(ListItem::new(Line::from(vec![
                    Span::styled(conn, conn_style),
                    Span::styled(name, name_style),
                ])));
            }
        }
    }

    let height = area.height as usize;
    let offset = if height > 0 && active_row_idx >= height {
        active_row_idx + 1 - height
    } else {
        0
    };
    let visible_items: Vec<ListItem> = items.into_iter().skip(offset).collect();

    let list = List::new(visible_items).block(Block::default().borders(Borders::RIGHT));
    f.render_widget(list, area);
}

pub fn compute_max_scroll(
    buf: &Buffer,
    my_nick: &str,
    width: usize,
    viewport_rows: usize,
) -> usize {
    if buf.messages.is_empty() {
        return 0;
    }
    let target = viewport_rows.max(1);
    let mut rows = 0usize;
    let mut top_messages = 0usize;
    for msg in &buf.messages {
        let formatted = format_chat_line(msg, my_nick);
        rows += wrapped_line_height(&formatted, width.max(1));
        top_messages += 1;
        if rows >= target {
            break;
        }
    }
    buf.messages.len().saturating_sub(top_messages)
}

fn horizontal_pad(area: Rect) -> Rect {
    if area.width > 2 {
        Rect::new(area.x + 1, area.y, area.width - 2, area.height)
    } else {
        area
    }
}

fn draw_chat_pane(f: &mut Frame, app: &App, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let buf = app.active_buf();
    let my_nick = app.active_nick();

    let padded = horizontal_pad(area);

    let inner_height = padded.height as usize;
    let width = padded.width.max(1) as usize;
    let total_msgs = buf.messages.len();
    let max_scroll = compute_max_scroll(buf, my_nick, width, inner_height);
    let scroll = buf.scroll_offset.min(max_scroll);
    let end = total_msgs.saturating_sub(scroll);

    let mut acc_rows = 0usize;
    let mut visible_lines = Vec::with_capacity(inner_height.min(end));
    for msg in buf.messages.range(..end).rev() {
        if acc_rows >= inner_height {
            break;
        }
        let formatted = format_chat_line(msg, my_nick);
        acc_rows += wrapped_line_height(&formatted, width);
        visible_lines.push(formatted);
    }
    visible_lines.reverse();

    let extra_scroll = acc_rows.saturating_sub(inner_height) as u16;
    let paragraph = Paragraph::new(visible_lines)
        .block(Block::default().borders(Borders::NONE))
        .wrap(Wrap { trim: false })
        .scroll((extra_scroll, 0));

    f.render_widget(paragraph, padded);
}

pub fn wrapped_line_height(line: &Line, width: usize) -> usize {
    if width == 0 {
        return 1;
    }

    let mut wrapped_lines = 0usize;
    let mut line_width = 0usize;
    let mut word_width = 0usize;
    let mut whitespace_width = 0usize;
    let mut pending_ws: std::collections::VecDeque<usize> = std::collections::VecDeque::new();
    let mut non_whitespace_previous = false;
    let mut has_any = false;

    for grapheme in line.styled_graphemes(Style::default()) {
        has_any = true;
        let symbol = grapheme.symbol;
        let is_whitespace =
            symbol == "\u{200b}" || (symbol.chars().all(char::is_whitespace) && symbol != "\u{00a0}");
        let symbol_width = UnicodeWidthStr::width(symbol);

        if symbol_width > width {
            continue;
        }

        let word_found = non_whitespace_previous && is_whitespace;
        let untrimmed_overflow = line_width == 0
            && word_width + whitespace_width + symbol_width > width;

        if word_found || untrimmed_overflow {
            line_width += whitespace_width + word_width;
            pending_ws.clear();
            whitespace_width = 0;
            word_width = 0;
        }

        let line_full = line_width >= width;
        let pending_word_overflow =
            symbol_width > 0 && line_width + whitespace_width + word_width >= width;

        if line_full || pending_word_overflow {
            let mut remaining_width = width.saturating_sub(line_width);
            wrapped_lines += 1;
            line_width = 0;

            while let Some(&ws_w) = pending_ws.front() {
                if ws_w > remaining_width {
                    break;
                }
                whitespace_width -= ws_w;
                remaining_width -= ws_w;
                pending_ws.pop_front();
            }

            if is_whitespace && pending_ws.is_empty() {
                continue;
            }
        }

        if is_whitespace {
            whitespace_width += symbol_width;
            pending_ws.push_back(symbol_width);
        } else {
            word_width += symbol_width;
        }

        non_whitespace_previous = !is_whitespace;
    }

    if !has_any {
        return 1;
    }

    if line_width > 0 || word_width > 0 || whitespace_width > 0 || !pending_ws.is_empty() {
        wrapped_lines += 1;
    }

    wrapped_lines.max(1)
}

fn format_chat_line(line: &ChatLine, my_nick: &str) -> Line<'static> {
    let mut spans = vec![Span::styled(
        format!("[{}] ", line.timestamp),
        Style::default().fg(Color::Gray),
    )];

    match line.kind {
        LineKind::Normal => {
            let is_self = line
                .sender
                .as_deref()
                .map(|s| s.eq_ignore_ascii_case(my_nick))
                .unwrap_or(false);
            let mentioned = !is_self && contains_nick_mention(&line.content, my_nick);

            if let Some(sender) = &line.sender {
                let n_color = if is_self {
                    Color::Cyan
                } else if mentioned {
                    Color::Yellow
                } else {
                    nick_color(sender)
                };
                spans.push(Span::styled("<", Style::default().fg(Color::Gray)));
                spans.push(Span::styled(
                    sender.clone(),
                    Style::default().fg(n_color).add_modifier(Modifier::BOLD),
                ));
                spans.push(Span::styled("> ", Style::default().fg(Color::Gray)));
            }
            let body_style = if mentioned {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Reset)
            };
            spans.extend(parse_irc_spans(&line.content, body_style));
        }
        LineKind::Action => {
            let actor = line.sender.as_deref().unwrap_or("*");
            let action_style = Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::ITALIC);
            spans.push(Span::styled(format!("* {actor} "), action_style));
            spans.extend(parse_irc_spans(&line.content, action_style));
        }
        LineKind::System => {
            let sys_style = if line.content.starts_with("-->") {
                Style::default().fg(Color::Green)
            } else if line.content.starts_with("<--") {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::Cyan)
            };
            spans.extend(parse_irc_spans(&line.content, sys_style));
        }
        LineKind::Error => {
            let err_style = Style::default()
                .fg(Color::LightRed)
                .add_modifier(Modifier::BOLD);
            spans.extend(parse_irc_spans(&line.content, err_style));
        }
    }

    Line::from(spans)
}

fn fit_input_prompt(nick: &str, server_id: &str, buf_name: &str, max_width: usize) -> String {
    let label = format!("{nick} @ {server_id}/{buf_name}");
    let full = format!(" [{label}] > ");
    if UnicodeWidthStr::width(full.as_str()) <= max_width {
        return full;
    }
    if max_width == 0 {
        return String::new();
    }
    if max_width == 1 {
        return ">".to_string();
    }
    if max_width < 7 {
        return "> ".to_string();
    }
    let budget = max_width - 7;
    let mut trimmed = String::new();
    let mut used = 0usize;
    for ch in label.chars() {
        let w = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + w > budget {
            break;
        }
        trimmed.push(ch);
        used += w;
    }
    format!(" [{trimmed}…] > ")
}

fn draw_input_bar(f: &mut Frame, app: &App, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let buf = app.active_buf();
    let nick = app.active_nick();
    let min_input_width = ((area.width as usize) / 2).clamp(1, 12);
    let max_prompt_width = (area.width as usize).saturating_sub(min_input_width);
    let prompt = fit_input_prompt(nick, &buf.server_id, &buf.name, max_prompt_width);
    let prompt_width = UnicodeWidthStr::width(prompt.as_str());

    let avail_width = (area.width as usize).saturating_sub(prompt_width);
    let before_cursor = &app.input.text[..app.input.cursor_pos];
    let cursor_col = UnicodeWidthStr::width(before_cursor);

    let mut skipped_bytes = 0usize;
    let mut skipped_width = 0usize;
    if avail_width > 0 && cursor_col >= avail_width {
        let target_skip = cursor_col - avail_width + 1;
        for (idx, ch) in before_cursor.char_indices() {
            if skipped_width >= target_skip {
                break;
            }
            skipped_width += UnicodeWidthChar::width(ch).unwrap_or(0);
            skipped_bytes = idx + ch.len_utf8();
        }
    }

    let visible_input = &app.input.text[skipped_bytes..];
    let line = Line::from(vec![
        Span::styled(
            prompt,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(visible_input.to_string()),
    ]);
    f.render_widget(Paragraph::new(line), area);

    if !app.show_help && area.width > 0 && area.height > 0 {
        let cursor_offset = cursor_col.saturating_sub(skipped_width);
        let cursor_x = area
            .x
            .saturating_add(prompt_width as u16)
            .saturating_add(cursor_offset as u16)
            .min(area.right().saturating_sub(1));
        f.set_cursor_position(Position::new(cursor_x, area.y));
    }
}

pub fn format_user_list_entry(entry: &str) -> (String, Color) {
    let (prefix, raw_nick) = strip_user_prefix(entry);
    match prefix {
        Some('~') => (format!(" {raw_nick}"), Color::Red),
        Some('&') => (format!(" {raw_nick}"), Color::Rgb(255, 165, 0)),
        Some('@') => (format!(" {raw_nick}"), Color::Yellow),
        Some('%') => (format!(" {raw_nick}"), Color::Green),
        Some('+') => (format!(" {raw_nick} (+)"), Color::White),
        _ => (format!(" {raw_nick}"), Color::White),
    }
}

fn draw_users_sidebar(f: &mut Frame, app: &App, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let buf = app.active_buf();
    let count = buf.users.len();
    let max_users = (area.height as usize).saturating_sub(1);

    let mut items: Vec<ListItem> = Vec::with_capacity(max_users.min(count) + 1);
    items.push(ListItem::new(Line::from(Span::styled(
        format!(" Users ({count})"),
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ))));

    for entry in buf.users.iter().skip(buf.user_scroll_offset).take(max_users) {
        let (label, color) = format_user_list_entry(entry);
        items.push(ListItem::new(Line::from(Span::styled(
            label,
            Style::default().fg(color),
        ))));
    }

    let list = List::new(items).block(Block::default().borders(Borders::LEFT));
    f.render_widget(list, area);
}

pub fn help_sections() -> Vec<(&'static str, Vec<(&'static str, &'static str)>)> {
    vec![
        (
            "Tree & Buffer Navigation",
            vec![
                ("Ctrl+n / Alt+Down", "Next buffer in tree"),
                ("Ctrl+p / Alt+Up", "Previous buffer in tree"),
                ("Alt+a", "Jump to next unread buffer"),
                ("Alt+1 .. Alt+9", "Jump to buffer 1..9"),
            ],
        ),
        (
            "Scrolling (Chat & Users)",
            vec![
                ("PgUp / PgDn", "Scroll chat history up / down"),
                ("Alt+PgUp / PgDn", "Scroll Users list up / down"),
                ("Esc", "Snap chat scrollback to bottom"),
            ],
        ),
        (
            "Input & Completion",
            vec![
                ("Tab", "Complete nick or /command"),
                ("Up / Down", "Previous / next sent message"),
                ("Left / Right", "Move cursor one character"),
                ("Home / Ctrl+a", "Move cursor to start of line"),
                ("End / Ctrl+e", "Move cursor to end of line"),
                ("Ctrl+w", "Delete word before cursor"),
                ("Ctrl+u", "Clear entire input line"),
                ("Ctrl+c", "Cancel current input line"),
                ("Enter", "Send message or /command"),
            ],
        ),
        (
            "Slash Commands",
            COMMAND_SPECS
                .iter()
                .map(|spec| (spec.usage, spec.description))
                .collect(),
        ),
        (
            "Help Dialog & App",
            vec![
                ("j / k / Up / Down", "Scroll help one line"),
                ("PgUp / PgDn", "Scroll help one page"),
                ("g / G", "Top / bottom of help"),
                ("Esc / q / Enter", "Close help dialog"),
                ("Ctrl+q", "Quit ronf"),
            ],
        ),
    ]
}

pub fn build_help_lines() -> (Vec<Line<'static>>, u16) {
    let sections = help_sections();
    let key_col = sections
        .iter()
        .flat_map(|(_, entries)| entries.iter().map(|(k, _)| k.chars().count()))
        .max()
        .unwrap_or(16)
        + 3;

    let mut max_w = 0usize;
    for (header, entries) in &sections {
        max_w = max_w.max(header.chars().count());
        for (_, desc) in entries {
            max_w = max_w.max(key_col + desc.chars().count());
        }
    }

    let mut lines = Vec::new();
    for (i, (header, entries)) in sections.iter().enumerate() {
        if i > 0 {
            lines.push(Line::from(""));
        }

        lines.push(Line::from(Span::styled(
            (*header).to_string(),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            "─".repeat(max_w),
            Style::default().fg(Color::Gray),
        )));

        for (key, desc) in entries {
            let key_span = Span::styled(
                format!("{:<key_col$}", key),
                Style::default().fg(Color::Cyan),
            );
            let desc_span = Span::raw((*desc).to_string());
            lines.push(Line::from(vec![key_span, desc_span]));
        }
    }

    (lines, max_w as u16)
}

fn cached_help() -> &'static (Vec<Line<'static>>, u16) {
    static HELP: OnceLock<(Vec<Line<'static>>, u16)> = OnceLock::new();
    HELP.get_or_init(build_help_lines)
}

pub fn help_line_count() -> usize {
    cached_help().0.len()
}

pub fn help_popup_height(area_height: u16) -> u16 {
    let total_lines = help_line_count() as u16;
    (total_lines + 2)
        .min(area_height.saturating_sub(2).max(6))
        .min(area_height)
}

pub fn help_max_scroll(area_height: u16) -> usize {
    let inner_h = help_popup_height(area_height).saturating_sub(2) as usize;
    help_line_count().saturating_sub(inner_h)
}

fn draw_help_popup(f: &mut Frame, app: &App) {
    let area = f.area();
    if area.width == 0 || area.height == 0 {
        return;
    }
    let (lines, content_width) = cached_help();

    let title = " Help — q/Esc to close, j/k to scroll ";
    let title_len = title.chars().count() as u16;
    let popup_w = (*content_width + 4).max(title_len + 2).min(area.width);
    let popup_h = help_popup_height(area.height);

    let x = area.width.saturating_sub(popup_w) / 2;
    let y = area.height.saturating_sub(popup_h) / 2;
    let popup_area = Rect::new(x, y, popup_w, popup_h);

    f.render_widget(Clear, popup_area);

    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        title,
        Style::default().add_modifier(Modifier::BOLD),
    ));
    let inner = block.inner(popup_area);
    f.render_widget(block, popup_area);

    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let padded = horizontal_pad(inner);

    let max_scroll = help_max_scroll(area.height);
    let scroll = app.help_scroll.min(max_scroll) as u16;

    let paragraph = Paragraph::new(lines.clone()).scroll((scroll, 0));
    f.render_widget(paragraph, padded);
}

/// Parses mIRC formatting codes (`\x02` bold, `\x1d` italic, `\x1f` underline,
/// `\x0f` reset, and `\x03[FG[,BG]]` colors 00..15) into styled `ratatui` spans.
pub fn parse_irc_spans(text: &str, base_style: Style) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut current_style = base_style;
    let mut current_text = String::new();

    let bytes = text.as_bytes();
    let mut i = 0;

    let flush = |spans: &mut Vec<Span<'static>>, buf: &mut String, style: Style| {
        if !buf.is_empty() {
            spans.push(Span::styled(std::mem::take(buf), style));
        }
    };

    while i < bytes.len() {
        match bytes[i] {
            0x02 | 0x16 | 0x1d | 0x1e | 0x1f => {
                flush(&mut spans, &mut current_text, current_style);
                let modifier = match bytes[i] {
                    0x02 => Modifier::BOLD,
                    0x16 => Modifier::REVERSED,
                    0x1d => Modifier::ITALIC,
                    0x1e => Modifier::CROSSED_OUT,
                    _ => Modifier::UNDERLINED,
                };
                current_style = toggle_modifier(current_style, modifier);
                i += 1;
            }
            0x11 => {
                i += 1;
            }
            0x0f => {
                flush(&mut spans, &mut current_text, current_style);
                current_style = base_style;
                i += 1;
            }
            0x03 => {
                flush(&mut spans, &mut current_text, current_style);
                i += 1;
                let (fg_code, consumed_fg) = parse_two_digit_color(&bytes[i..]);
                i += consumed_fg;
                let mut consumed_bg = 0usize;
                let mut bg_code = None;
                if i < bytes.len() && bytes[i] == b',' {
                    let (parsed_bg, len_bg) = parse_two_digit_color(&bytes[i + 1..]);
                    if len_bg > 0 {
                        consumed_bg = len_bg;
                        bg_code = parsed_bg;
                        i += 1 + len_bg;
                    }
                }
                if consumed_fg > 0 || consumed_bg > 0 {
                    if let Some(fg) = fg_code {
                        if fg == 99 {
                            current_style.fg = base_style.fg;
                        } else if let Some(c) = mirc_color_to_ratatui(fg) {
                            current_style = current_style.fg(c);
                        }
                    }
                    if let Some(bg) = bg_code {
                        if bg == 99 {
                            current_style.bg = base_style.bg;
                        } else if let Some(c) = mirc_color_to_ratatui(bg) {
                            current_style = current_style.bg(c);
                        }
                    }
                } else {
                    current_style.fg = base_style.fg;
                    current_style.bg = base_style.bg;
                }
            }
            _ => {
                let ch = text[i..].chars().next().unwrap();
                current_text.push(ch);
                i += ch.len_utf8();
            }
        }
    }

    flush(&mut spans, &mut current_text, current_style);
    if spans.is_empty() {
        spans.push(Span::styled(String::new(), base_style));
    }
    spans
}

fn parse_two_digit_color(bytes: &[u8]) -> (Option<u8>, usize) {
    if bytes.is_empty() || !bytes[0].is_ascii_digit() {
        return (None, 0);
    }
    let d1 = bytes[0] - b'0';
    if bytes.len() >= 2 && bytes[1].is_ascii_digit() {
        let d2 = bytes[1] - b'0';
        (Some(d1 * 10 + d2), 2)
    } else {
        (Some(d1), 1)
    }
}

fn toggle_modifier(style: Style, modifier: Modifier) -> Style {
    if style.add_modifier.contains(modifier) {
        style.remove_modifier(modifier)
    } else {
        style.add_modifier(modifier)
    }
}

const MIRC_COLORS: [Color; 16] = [
    Color::White,
    Color::Black,
    Color::Blue,
    Color::Green,
    Color::LightRed,
    Color::Red,
    Color::Magenta,
    Color::Yellow,
    Color::LightYellow,
    Color::LightGreen,
    Color::Cyan,
    Color::LightCyan,
    Color::LightBlue,
    Color::LightMagenta,
    Color::Gray,
    Color::Gray,
];

fn mirc_color_to_ratatui(code: u8) -> Option<Color> {
    if let Some(&c) = MIRC_COLORS.get(code as usize) {
        Some(c)
    } else if (16..=98).contains(&code) {
        Some(Color::Indexed(code))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;

    #[test]
    fn builds_tree_rows_with_connectors_and_separators() {
        let toml = r##"
            default_nickname = "irc_user"

            [servers.libera]
            server = "irc.libera.chat"
            channels = ["#linux", "#rust"]

            [servers.oftc]
            server = "irc.oftc.net"
            channels = ["#debian"]
        "##;
        let cfg = AppConfig::from_toml_str(toml).unwrap();
        let app = App::from_config(&cfg);
        let rows = build_tree_rows(&app);

        assert_eq!(
            rows,
            vec![
                TreeRow::Server {
                    name: "libera",
                    active: true,
                    unread: false,
                },
                TreeRow::Child {
                    name: "#linux",
                    active: false,
                    unread: false,
                    highlight: false,
                },
                TreeRow::Child {
                    name: "#rust",
                    active: false,
                    unread: false,
                    highlight: false,
                },
                TreeRow::Separator,
                TreeRow::Server {
                    name: "oftc",
                    active: false,
                    unread: false,
                },
                TreeRow::Child {
                    name: "#debian",
                    active: false,
                    unread: false,
                    highlight: false,
                },
            ]
        );
    }

    #[test]
    fn deterministic_nick_color_ignores_role_prefix_and_case() {
        assert_eq!(nick_color("alice"), nick_color("@Alice"));
        assert_eq!(nick_color("ferris"), nick_color("+FERRIS"));
    }

    #[test]
    fn formats_user_list_entries_by_role_rank_and_color() {
        assert_eq!(
            format_user_list_entry("~owner"),
            (" owner".to_string(), Color::Red)
        );
        assert_eq!(
            format_user_list_entry("&admin"),
            (" admin".to_string(), Color::Rgb(255, 165, 0))
        );
        assert_eq!(
            format_user_list_entry("@op"),
            (" op".to_string(), Color::Yellow)
        );
        assert_eq!(
            format_user_list_entry("%halfop"),
            (" halfop".to_string(), Color::Green)
        );
        assert_eq!(
            format_user_list_entry("+voiced"),
            (" voiced (+)".to_string(), Color::White)
        );
        assert_eq!(
            format_user_list_entry("regular"),
            (" regular".to_string(), Color::White)
        );
    }

    #[test]
    fn computes_wrapped_line_height_accurately() {
        let short = Line::from("hello world");
        assert_eq!(wrapped_line_height(&short, 80), 1);

        let long = Line::from("a".repeat(165));
        assert_eq!(wrapped_line_height(&long, 80), 3);

        let word_wrapped = Line::from("aaaaa bbbbb ccccc");
        assert_eq!(wrapped_line_height(&word_wrapped, 10), 3);

        let combining = Line::from("e\u{0301}".repeat(10));
        assert_eq!(wrapped_line_height(&combining, 10), 1);

        let cjk = Line::from("你好世界你好");
        assert_eq!(wrapped_line_height(&cjk, 10), 2);
    }

    #[test]
    fn parses_mirc_bold_color_and_reset_codes() {
        let base = Style::default();
        let input = "\x02bold\x02 \x0304,01red_on_black\x0f plain";
        let spans = parse_irc_spans(input, base);

        assert_eq!(spans.len(), 4);
        assert_eq!(spans[0].content, "bold");
        assert!(spans[0].style.add_modifier.contains(Modifier::BOLD));

        assert_eq!(spans[1].content, " ");
        assert!(!spans[1].style.add_modifier.contains(Modifier::BOLD));

        assert_eq!(spans[2].content, "red_on_black");
        assert_eq!(spans[2].style.fg, Some(Color::LightRed));
        assert_eq!(spans[2].style.bg, Some(Color::Black));

        assert_eq!(spans[3].content, " plain");
        assert_eq!(spans[3].style, base);

        let bg_and_99 = parse_irc_spans("\x0304,02colored\x03,99bg_reset\x16rev\x1estrike", base);
        assert_eq!(bg_and_99[0].style.fg, Some(Color::LightRed));
        assert_eq!(bg_and_99[0].style.bg, Some(Color::Blue));
        assert_eq!(bg_and_99[1].content, "bg_reset");
        assert_eq!(bg_and_99[1].style.fg, Some(Color::LightRed));
        assert_eq!(bg_and_99[1].style.bg, None);
        assert!(bg_and_99[2].style.add_modifier.contains(Modifier::REVERSED));
        assert!(bg_and_99[3].style.add_modifier.contains(Modifier::CROSSED_OUT));
    }

    #[test]
    fn help_lines_contain_organized_sections_and_aligned_columns() {
        let (lines, width) = build_help_lines();
        assert!(width > 30 && width < 60);
        let combined: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(combined.contains("Tree & Buffer Navigation"));
        assert!(combined.contains("Scrolling (Chat & Users)"));
        assert!(combined.contains("Input & Completion"));
        assert!(combined.contains("Slash Commands"));
        assert!(combined.contains("/help"));

        // Verify every key span has trailing spaces (no overlap with description)
        for line in &lines {
            if line.spans.len() == 2 {
                assert!(line.spans[0].content.ends_with("   ") || line.spans[0].content.ends_with("  "));
            }
        }
    }

    #[test]
    fn draws_safely_on_tiny_terminal_dimensions() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let toml = r##"
            default_nickname = "irc_user"
            [servers.libera]
            server = "irc.libera.chat"
            channels = ["#rust"]
        "##;
        let cfg = AppConfig::from_toml_str(toml).unwrap();
        let mut app = App::from_config(&cfg);
        app.show_help = true;

        for (w, h) in [(1, 1), (2, 2), (10, 3), (44, 5)] {
            app.set_terminal_size(w, h);
            let backend = TestBackend::new(w, h);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal.draw(|f| draw(f, &app)).unwrap();
        }
    }

    #[test]
    fn input_bar_keeps_typed_text_visible_when_prompt_is_long() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let toml = r##"
            default_nickname = "my_nickname"
            [servers.libera]
            server = "irc.libera.chat"
            channels = ["#kernelnewbies"]
        "##;
        let cfg = AppConfig::from_toml_str(toml).unwrap();
        let mut app = App::from_config(&cfg);
        let idx = app.find_buffer("libera", "#kernelnewbies").unwrap();
        app.select_buffer(idx);
        app.set_terminal_size(80, 24);
        app.input.set_text("hello world");

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &app)).unwrap();

        let buf = terminal.backend().buffer();
        let input_row: String = (24..60)
            .map(|x| buf[(x, 23)].symbol())
            .collect();
        assert!(
            input_row.contains("hello world"),
            "Expected typed text to remain visible in input row: {input_row:?}"
        );
    }
}
