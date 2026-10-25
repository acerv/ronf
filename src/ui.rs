// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::OnceLock;

use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::App;
use crate::buffer::{contains_nick_mention, strip_user_prefix, BufferKind, ChatLine, LineKind};
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

fn draw_chat_pane(f: &mut Frame, app: &App, area: Rect) {
    let buf = app.active_buf();
    let my_nick = app.active_nick();

    let padded = Rect::new(
        area.x.saturating_add(1),
        area.y,
        area.width.saturating_sub(2),
        area.height,
    );

    let inner_height = padded.height as usize;
    let width = padded.width.max(1) as usize;
    let total_msgs = buf.messages.len();
    let end = total_msgs.saturating_sub(buf.scroll_offset);

    let mut acc_rows = 0usize;
    let mut visible_lines = Vec::with_capacity(inner_height.min(end));
    for msg in buf.messages[..end].iter().rev() {
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
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    if text.is_empty() {
        return 1;
    }

    let mut rows = 1usize;
    let mut col = 0usize;
    let mut chars = text.chars().peekable();

    while let Some(&ch) = chars.peek() {
        let is_ws = ch.is_whitespace();
        let mut seg_len = 0usize;
        while let Some(&c) = chars.peek() {
            if c.is_whitespace() == is_ws {
                seg_len += char_display_width(c);
                chars.next();
            } else {
                break;
            }
        }

        if is_ws {
            if col + seg_len <= width {
                col += seg_len;
            } else {
                let rem = (col + seg_len) - width;
                rows += 1 + rem / width;
                col = rem % width;
            }
        } else if col > 0 && col + seg_len <= width {
            col += seg_len;
        } else {
            if col > 0 {
                rows += 1;
            }
            rows += (seg_len.saturating_sub(1)) / width;
            col = if seg_len == 0 {
                0
            } else {
                (seg_len - 1) % width + 1
            };
        }
    }

    rows
}

fn char_display_width(c: char) -> usize {
    let u = c as u32;
    if (0x1100..=0x115F).contains(&u)
        || (0x2E80..=0xA4CF).contains(&u)
        || (0xAC00..=0xD7A3).contains(&u)
        || (0xF900..=0xFAFF).contains(&u)
        || (0xFE10..=0xFE6F).contains(&u)
        || (0xFF00..=0xFF60).contains(&u)
        || (0x1F300..=0x1FAFF).contains(&u)
    {
        2
    } else {
        1
    }
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

fn draw_input_bar(f: &mut Frame, app: &App, area: Rect) {
    let buf = app.active_buf();
    let nick = app.active_nick();
    let prompt = format!(" [{nick} @ {}/{}] > ", buf.server_id, buf.name);
    let prompt_width = prompt.chars().count();

    let avail_width = (area.width as usize).saturating_sub(prompt_width);
    let cursor_chars = app.input.text[..app.input.cursor_pos].chars().count();
    let horizontal_scroll = if avail_width > 0 && cursor_chars >= avail_width {
        cursor_chars - avail_width + 1
    } else {
        0
    };

    let visible_input: String = app.input.text.chars().skip(horizontal_scroll).collect();
    let line = Line::from(vec![
        Span::styled(
            prompt,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(visible_input),
    ]);
    f.render_widget(Paragraph::new(line), area);

    if !app.show_help {
        let cursor_x = area
            .x
            .saturating_add(prompt_width as u16)
            .saturating_add((cursor_chars - horizontal_scroll) as u16)
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
    let buf = app.active_buf();
    let count = buf.users.len();

    let mut items: Vec<ListItem> = Vec::with_capacity(count + 2);
    items.push(ListItem::new(Line::from(Span::styled(
        format!(" Users ({count})"),
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ))));

    for entry in buf.users.iter().skip(buf.user_scroll_offset) {
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

    let padded = Rect::new(
        inner.x + 1,
        inner.y,
        inner.width.saturating_sub(2),
        inner.height,
    );

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
            0x02 | 0x1d | 0x1f => {
                flush(&mut spans, &mut current_text, current_style);
                let modifier = match bytes[i] {
                    0x02 => Modifier::BOLD,
                    0x1d => Modifier::ITALIC,
                    _ => Modifier::UNDERLINED,
                };
                current_style = toggle_modifier(current_style, modifier);
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
                if let Some(fg) = fg_code {
                    if let Some(c) = mirc_color_to_ratatui(fg) {
                        current_style = current_style.fg(c);
                    }
                    if i < bytes.len() && bytes[i] == b',' {
                        let (bg_code, consumed_bg) = parse_two_digit_color(&bytes[i + 1..]);
                        if consumed_bg > 0 {
                            i += 1 + consumed_bg;
                            if let Some(bg) = bg_code.and_then(mirc_color_to_ratatui) {
                                current_style = current_style.bg(bg);
                            }
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
    MIRC_COLORS.get(code as usize).copied()
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
}
