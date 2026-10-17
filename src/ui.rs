// SPDX-License-Identifier: GPL-3.0-or-later

use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{strip_user_prefix, App, BufferKind, ChatLine, LineKind};

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
pub enum TreeRow {
    Server {
        buffer_idx: usize,
        name: String,
        active: bool,
        unread: bool,
    },
    Child {
        buffer_idx: usize,
        connector: &'static str,
        name: String,
        active: bool,
        unread: bool,
    },
    Separator,
}

pub fn build_tree_rows(app: &App) -> Vec<TreeRow> {
    let mut rows = Vec::new();
    let len = app.buffers.len();

    for i in 0..len {
        let buf = &app.buffers[i];
        let active = i == app.active_buffer;
        let unread = buf.unread;

        if buf.kind == BufferKind::Server {
            if !rows.is_empty() {
                rows.push(TreeRow::Separator);
            }
            rows.push(TreeRow::Server {
                buffer_idx: i,
                name: buf.name.clone(),
                active,
                unread,
            });
        } else {
            let is_last_for_server = (i + 1 >= len)
                || !app.buffers[i + 1]
                    .server_id
                    .eq_ignore_ascii_case(&buf.server_id)
                || app.buffers[i + 1].kind == BufferKind::Server;
            let connector = if is_last_for_server {
                " └─ "
            } else {
                " ├─ "
            };
            rows.push(TreeRow::Child {
                buffer_idx: i,
                connector,
                name: buf.name.clone(),
                active,
                unread,
            });
        }
    }

    rows
}

pub fn draw(f: &mut Frame, app: &App) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(24),
            Constraint::Min(1),
            Constraint::Length(20),
        ])
        .split(f.area());

    let center_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(3)])
        .split(columns[1]);

    draw_tree_sidebar(f, app, columns[0]);
    draw_chat_pane(f, app, center_chunks[0]);
    draw_input_box(f, app, center_chunks[1]);
    draw_users_sidebar(f, app, columns[2]);

    if app.show_help {
        draw_help_popup(f, app);
    }
}

fn draw_tree_sidebar(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let tree_rows = build_tree_rows(app);
    let mut items = Vec::with_capacity(tree_rows.len());

    for row in tree_rows {
        match row {
            TreeRow::Separator => {
                items.push(ListItem::new(Line::from("")));
            }
            TreeRow::Server {
                name,
                active,
                unread,
                ..
            } => {
                let mut spans = Vec::new();
                let style = if active {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD)
                };
                spans.push(Span::styled(format!(" {name}"), style));
                if active {
                    spans.push(Span::styled(
                        " ◂",
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ));
                } else if unread {
                    spans.push(Span::styled(
                        " *",
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ));
                }
                items.push(ListItem::new(Line::from(spans)));
            }
            TreeRow::Child {
                connector,
                name,
                active,
                unread,
                ..
            } => {
                let mut spans = vec![Span::styled(
                    connector,
                    Style::default().fg(Color::DarkGray),
                )];
                let name_style = if active {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else if unread {
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Gray)
                };
                spans.push(Span::styled(name, name_style));
                if active {
                    spans.push(Span::styled(
                        " ◂",
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ));
                } else if unread {
                    spans.push(Span::styled(
                        " *",
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ));
                }
                items.push(ListItem::new(Line::from(spans)));
            }
        }
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Blue))
        .title(Span::styled(
            " Servers & Channels ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));

    let list = List::new(items).block(block);
    f.render_widget(list, area);
}

fn draw_chat_pane(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let buf = app.active_buf();
    let my_nick = app.active_nick();

    let title = match (&buf.kind, &buf.topic) {
        (BufferKind::Server, _) => format!(" {} (Server) ", buf.server_id),
        (BufferKind::DirectMessage, _) => format!(" {} / DM: {} ", buf.server_id, buf.name),
        (BufferKind::Channel, Some(topic)) if !topic.is_empty() => {
            format!(" {} / {} — {} ", buf.server_id, buf.name, topic)
        }
        (BufferKind::Channel, _) => format!(" {} / {} ", buf.server_id, buf.name),
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Blue))
        .title(Span::styled(
            title,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ));

    let inner_height = area.height.saturating_sub(2) as usize;
    let total_msgs = buf.messages.len();
    let end = total_msgs.saturating_sub(buf.scroll_offset);
    let start = end.saturating_sub(inner_height);

    let visible_lines: Vec<Line> = buf.messages[start..end]
        .iter()
        .map(|msg| format_chat_line(msg, my_nick))
        .collect();

    let paragraph = Paragraph::new(visible_lines)
        .block(block)
        .wrap(Wrap { trim: false });

    f.render_widget(paragraph, area);
}

fn format_chat_line(line: &ChatLine, my_nick: &str) -> Line<'static> {
    let mut spans = vec![Span::styled(
        format!("[{}] ", line.timestamp),
        Style::default().fg(Color::DarkGray),
    )];

    match line.kind {
        LineKind::Normal => {
            if let Some(sender) = &line.sender {
                let n_color = if sender.eq_ignore_ascii_case(my_nick) {
                    Color::Cyan
                } else {
                    nick_color(sender)
                };
                spans.push(Span::styled("<", Style::default().fg(Color::DarkGray)));
                spans.push(Span::styled(
                    sender.clone(),
                    Style::default().fg(n_color).add_modifier(Modifier::BOLD),
                ));
                spans.push(Span::styled("> ", Style::default().fg(Color::DarkGray)));
            }
            spans.extend(parse_irc_spans(
                &line.content,
                Style::default().fg(Color::Reset),
            ));
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

fn draw_input_box(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let buf = app.active_buf();
    let nick = app.active_nick();
    let prompt_title = format!(" [{nick} @ {}/{}] ", buf.server_id, buf.name);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Blue))
        .title(Span::styled(
            prompt_title,
            Style::default()
                .fg(Color::LightGreen)
                .add_modifier(Modifier::BOLD),
        ));

    let inner_width = area.width.saturating_sub(2) as usize;
    let cursor_chars = app.input[..app.cursor_pos].chars().count();
    let horizontal_scroll = if inner_width > 0 && cursor_chars >= inner_width {
        cursor_chars - inner_width + 1
    } else {
        0
    };

    let visible_input: String = app.input.chars().skip(horizontal_scroll).collect();
    let paragraph = Paragraph::new(visible_input).block(block);
    f.render_widget(paragraph, area);

    if !app.show_help {
        let cursor_x = area
            .x
            .saturating_add(1)
            .saturating_add((cursor_chars - horizontal_scroll) as u16)
            .min(area.right().saturating_sub(2));
        let cursor_y = area.y.saturating_add(1);
        f.set_cursor_position(Position::new(cursor_x, cursor_y));
    }
}

fn draw_users_sidebar(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let buf = app.active_buf();
    let count = buf.users.len();
    let title = format!(" Users ({count}) ");

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Blue))
        .title(Span::styled(
            title,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));

    let items: Vec<ListItem> = buf
        .users
        .iter()
        .skip(buf.user_scroll_offset)
        .map(|entry| {
            let (prefix, raw_nick) = strip_user_prefix(entry);
            let (prefix_str, prefix_style) = match prefix {
                Some(p @ ('~' | '&' | '@')) => (
                    format!("{p}"),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Some(p @ ('%' | '+')) => (
                    format!("{p}"),
                    Style::default()
                        .fg(Color::LightGreen)
                        .add_modifier(Modifier::BOLD),
                ),
                _ => (" ".to_string(), Style::default()),
            };

            let n_color = nick_color(raw_nick);
            ListItem::new(Line::from(vec![
                Span::styled(prefix_str, prefix_style),
                Span::styled(raw_nick.to_string(), Style::default().fg(n_color)),
            ]))
        })
        .collect();

    let list = List::new(items).block(block);
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
                ("Enter", "Send message or /command"),
            ],
        ),
        (
            "Slash Commands",
            vec![
                ("/help", "Open this help dialog"),
                ("/join <#chan>", "Join channel on active server"),
                ("/part [reason]", "Leave current channel"),
                ("/msg <to> <text>", "Send private or channel message"),
                ("/nick <newnick>", "Change nick on active server"),
                ("/me <action>", "Send CTCP action message"),
                ("/close", "Close active channel or DM"),
                ("/quit [reason]", "Disconnect all and exit"),
            ],
        ),
        (
            "Help Dialog & App",
            vec![
                ("j / k / Up / Down", "Scroll help one line"),
                ("PgUp / PgDn", "Scroll help one page"),
                ("g / G", "Top / bottom of help"),
                ("Esc / q / Enter", "Close help dialog"),
                ("Ctrl+c / Ctrl+q", "Quit ronf"),
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
            Style::default().fg(Color::DarkGray),
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

fn draw_help_popup(f: &mut Frame, app: &App) {
    let area = f.area();
    let (lines, content_width) = build_help_lines();
    let total_lines = lines.len() as u16;

    let title = " Help — q/Esc to close, j/k to scroll ";
    let title_len = title.chars().count() as u16;
    let popup_w = (content_width + 4).max(title_len + 2).min(area.width);
    let popup_h = (total_lines + 2).min(area.height.saturating_sub(2).max(6));

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

    let max_scroll = (lines.len() as usize).saturating_sub(padded.height as usize);
    let scroll = app.help_scroll.min(max_scroll) as u16;

    let paragraph = Paragraph::new(lines).scroll((scroll, 0));
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
            0x02 => {
                flush(&mut spans, &mut current_text, current_style);
                if current_style.add_modifier.contains(Modifier::BOLD) {
                    current_style = current_style.remove_modifier(Modifier::BOLD);
                } else {
                    current_style = current_style.add_modifier(Modifier::BOLD);
                }
                i += 1;
            }
            0x1d => {
                flush(&mut spans, &mut current_text, current_style);
                if current_style.add_modifier.contains(Modifier::ITALIC) {
                    current_style = current_style.remove_modifier(Modifier::ITALIC);
                } else {
                    current_style = current_style.add_modifier(Modifier::ITALIC);
                }
                i += 1;
            }
            0x1f => {
                flush(&mut spans, &mut current_text, current_style);
                if current_style.add_modifier.contains(Modifier::UNDERLINED) {
                    current_style = current_style.remove_modifier(Modifier::UNDERLINED);
                } else {
                    current_style = current_style.add_modifier(Modifier::UNDERLINED);
                }
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

fn mirc_color_to_ratatui(code: u8) -> Option<Color> {
    match code {
        0 => Some(Color::White),
        1 => Some(Color::Black),
        2 => Some(Color::Blue),
        3 => Some(Color::Green),
        4 => Some(Color::LightRed),
        5 => Some(Color::Red),
        6 => Some(Color::Magenta),
        7 => Some(Color::Yellow),
        8 => Some(Color::LightYellow),
        9 => Some(Color::LightGreen),
        10 => Some(Color::Cyan),
        11 => Some(Color::LightCyan),
        12 => Some(Color::LightBlue),
        13 => Some(Color::LightMagenta),
        14 => Some(Color::DarkGray),
        15 => Some(Color::Gray),
        _ => None,
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
                    buffer_idx: 0,
                    name: "libera".into(),
                    active: true,
                    unread: false,
                },
                TreeRow::Child {
                    buffer_idx: 1,
                    connector: " ├─ ",
                    name: "#linux".into(),
                    active: false,
                    unread: false,
                },
                TreeRow::Child {
                    buffer_idx: 2,
                    connector: " └─ ",
                    name: "#rust".into(),
                    active: false,
                    unread: false,
                },
                TreeRow::Separator,
                TreeRow::Server {
                    buffer_idx: 3,
                    name: "oftc".into(),
                    active: false,
                    unread: false,
                },
                TreeRow::Child {
                    buffer_idx: 4,
                    connector: " └─ ",
                    name: "#debian".into(),
                    active: false,
                    unread: false,
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
