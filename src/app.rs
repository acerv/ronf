// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::{BTreeMap, HashMap};

use chrono::Local;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use irc::client::Sender;
use irc::proto::{Command, Message, Prefix, Response};

use crate::config::AppConfig;

pub const SLASH_COMMANDS: &[&str] = &[
    "/help", "/join", "/part", "/msg", "/nick", "/me", "/topic", "/whois", "/close", "/quit",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Normal,
    Action,
    System,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatLine {
    pub timestamp: String,
    pub sender: Option<String>,
    pub content: String,
    pub kind: LineKind,
}

impl ChatLine {
    pub fn new(sender: Option<String>, content: impl Into<String>, kind: LineKind) -> Self {
        Self {
            timestamp: Local::now().format("%H:%M:%S").to_string(),
            sender,
            content: content.into(),
            kind,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufferKind {
    Server,
    Channel,
    DirectMessage,
}

#[derive(Debug, Clone)]
pub struct Buffer {
    pub server_id: String,
    pub name: String,
    pub kind: BufferKind,
    pub topic: Option<String>,
    pub users: Vec<String>,
    pub pending_names: Vec<String>,
    pub user_scroll_offset: usize,
    pub messages: Vec<ChatLine>,
    pub scroll_offset: usize,
    pub unread: bool,
}

impl Buffer {
    pub fn new(server_id: impl Into<String>, name: impl Into<String>, kind: BufferKind) -> Self {
        Self {
            server_id: server_id.into(),
            name: name.into(),
            kind,
            topic: None,
            users: Vec::new(),
            pending_names: Vec::new(),
            user_scroll_offset: 0,
            messages: Vec::new(),
            scroll_offset: 0,
            unread: false,
        }
    }

    pub fn push_line(&mut self, line: ChatLine, is_active: bool) {
        self.messages.push(line);
        if !is_active {
            self.unread = true;
        }
    }

    pub fn sort_users(&mut self) {
        self.users.sort_by(|a, b| {
            let (pref_a, nick_a) = strip_user_prefix(a);
            let (pref_b, nick_b) = strip_user_prefix(b);
            role_rank(pref_a)
                .cmp(&role_rank(pref_b))
                .then_with(|| nick_a.to_ascii_lowercase().cmp(&nick_b.to_ascii_lowercase()))
        });
        self.users.dedup_by(|a, b| {
            let (_, nick_a) = strip_user_prefix(a);
            let (_, nick_b) = strip_user_prefix(b);
            nick_a.eq_ignore_ascii_case(nick_b)
        });
    }

    pub fn add_user(&mut self, entry: &str) {
        let (_, new_nick) = strip_user_prefix(entry);
        if new_nick.is_empty() {
            return;
        }
        if let Some(existing) = self.users.iter_mut().find(|u| {
            let (_, n) = strip_user_prefix(u);
            n.eq_ignore_ascii_case(new_nick)
        }) {
            *existing = entry.to_string();
        } else {
            self.users.push(entry.to_string());
        }
        self.sort_users();
    }

    pub fn remove_user(&mut self, nick: &str) -> bool {
        let (_, target_nick) = strip_user_prefix(nick);
        let before = self.users.len();
        self.users.retain(|u| {
            let (_, n) = strip_user_prefix(u);
            !n.eq_ignore_ascii_case(target_nick)
        });
        before != self.users.len()
    }

    pub fn rename_user(&mut self, old_nick: &str, new_nick: &str) -> bool {
        let (_, old_raw) = strip_user_prefix(old_nick);
        let (_, new_raw) = strip_user_prefix(new_nick);
        let mut found = false;
        for user in &mut self.users {
            let (prefix, n) = strip_user_prefix(user);
            if n.eq_ignore_ascii_case(old_raw) {
                *user = match prefix {
                    Some(p) => format!("{p}{new_raw}"),
                    None => new_raw.to_string(),
                };
                found = true;
            }
        }
        if found {
            self.sort_users();
        }
        found
    }
}

pub fn strip_user_prefix(entry: &str) -> (Option<char>, &str) {
    let mut chars = entry.chars();
    if let Some(first) = chars.next() {
        if matches!(first, '~' | '&' | '@' | '%' | '+') {
            return (Some(first), chars.as_str());
        }
    }
    (None, entry)
}

fn role_rank(prefix: Option<char>) -> u8 {
    match prefix {
        Some('~') => 0,
        Some('&') => 1,
        Some('@') => 2,
        Some('%') => 3,
        Some('+') => 4,
        _ => 5,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserCommand {
    Help,
    Join(String),
    Part(Option<String>),
    Msg { target: String, text: String },
    Nick(String),
    Me(String),
    Topic(Option<String>),
    Whois(String),
    Close,
    Quit(Option<String>),
    Say(String),
}

pub fn parse_user_command(input: &str) -> Option<UserCommand> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }

    if !trimmed.starts_with('/') {
        return Some(UserCommand::Say(input.to_string()));
    }

    let (cmd, rest) = match trimmed.split_once(char::is_whitespace) {
        Some((c, r)) => (c.to_ascii_lowercase(), r.trim()),
        None => (trimmed.to_ascii_lowercase(), ""),
    };

    match cmd.as_str() {
        "/help" | "/h" | "/?" => Some(UserCommand::Help),
        "/join" | "/j" => {
            if rest.is_empty() {
                None
            } else {
                let chan = rest.split_whitespace().next().unwrap_or(rest);
                let normalized = if chan.starts_with('#') || chan.starts_with('&') {
                    chan.to_string()
                } else {
                    format!("#{chan}")
                };
                Some(UserCommand::Join(normalized))
            }
        }
        "/part" | "/leave" => {
            let reason = if rest.is_empty() {
                None
            } else {
                Some(rest.to_string())
            };
            Some(UserCommand::Part(reason))
        }
        "/msg" | "/query" => {
            let (target, text) = rest.split_once(char::is_whitespace)?;
            let text = text.trim();
            if target.is_empty() || text.is_empty() {
                None
            } else {
                Some(UserCommand::Msg {
                    target: target.to_string(),
                    text: text.to_string(),
                })
            }
        }
        "/nick" => {
            let new_nick = rest.split_whitespace().next()?;
            if new_nick.is_empty() {
                None
            } else {
                Some(UserCommand::Nick(new_nick.to_string()))
            }
        }
        "/me" => {
            if rest.is_empty() {
                None
            } else {
                Some(UserCommand::Me(rest.to_string()))
            }
        }
        "/topic" => {
            let new_topic = if rest.is_empty() {
                None
            } else {
                Some(rest.to_string())
            };
            Some(UserCommand::Topic(new_topic))
        }
        "/whois" => {
            let target = rest.split_whitespace().next()?;
            if target.is_empty() {
                None
            } else {
                Some(UserCommand::Whois(target.to_string()))
            }
        }
        "/close" => Some(UserCommand::Close),
        "/quit" | "/exit" => {
            let reason = if rest.is_empty() {
                None
            } else {
                Some(rest.to_string())
            };
            Some(UserCommand::Quit(reason))
        }
        _ => None,
    }
}

#[derive(Debug, Clone)]
pub struct TabCompletionState {
    pub replace_start: usize,
    pub replace_len: usize,
    pub candidates: Vec<String>,
    pub index: usize,
}

#[derive(Debug)]
pub enum ServerEvent {
    Connected {
        server_id: String,
        sender: Sender,
    },
    Message {
        server_id: String,
        message: Message,
    },
    Error {
        server_id: String,
        error: String,
    },
}

pub struct App {
    pub buffers: Vec<Buffer>,
    pub active_buffer: usize,
    pub nicks: BTreeMap<String, String>,
    pub senders: HashMap<String, Sender>,
    pub input: String,
    pub cursor_pos: usize,
    pub input_history: Vec<String>,
    pub history_index: Option<usize>,
    pub saved_draft: String,
    pub tab_state: Option<TabCompletionState>,
    pub show_help: bool,
    pub help_scroll: usize,
    pub should_quit: bool,
}

impl App {
    pub fn from_config(config: &AppConfig) -> Self {
        let mut buffers = Vec::new();
        let mut nicks = BTreeMap::new();

        for (server_id, srv) in config.enabled_servers() {
            let nick = srv
                .effective_nickname(config.default_nickname.as_deref())
                .unwrap_or_else(|_| "irc_user".to_string());
            nicks.insert(server_id.clone(), nick);

            let mut srv_buf = Buffer::new(server_id.clone(), server_id.clone(), BufferKind::Server);
            srv_buf.push_line(
                ChatLine::new(
                    None,
                    format!(
                        "Connecting to {}:{} (TLS: {})...",
                        srv.server,
                        srv.effective_port(),
                        srv.effective_tls()
                    ),
                    LineKind::System,
                ),
                true,
            );
            buffers.push(srv_buf);

            for chan in &srv.channels {
                buffers.push(Buffer::new(
                    server_id.clone(),
                    chan.clone(),
                    BufferKind::Channel,
                ));
            }
        }

        Self {
            buffers,
            active_buffer: 0,
            nicks,
            senders: HashMap::new(),
            input: String::new(),
            cursor_pos: 0,
            input_history: Vec::new(),
            history_index: None,
            saved_draft: String::new(),
            tab_state: None,
            show_help: false,
            help_scroll: 0,
            should_quit: false,
        }
    }

    pub fn active_buf(&self) -> &Buffer {
        &self.buffers[self.active_buffer]
    }

    pub fn active_buf_mut(&mut self) -> &mut Buffer {
        &mut self.buffers[self.active_buffer]
    }

    pub fn active_server_id(&self) -> String {
        self.active_buf().server_id.clone()
    }

    pub fn active_nick(&self) -> &str {
        let srv = &self.active_buf().server_id;
        self.nicks.get(srv).map(String::as_str).unwrap_or("irc_user")
    }

    pub fn select_buffer(&mut self, index: usize) {
        if index < self.buffers.len() {
            self.active_buffer = index;
            self.buffers[index].unread = false;
            self.buffers[index].scroll_offset = 0;
        }
    }

    pub fn next_buffer(&mut self) {
        if !self.buffers.is_empty() {
            let next = (self.active_buffer + 1) % self.buffers.len();
            self.select_buffer(next);
        }
    }

    pub fn prev_buffer(&mut self) {
        if !self.buffers.is_empty() {
            let prev = if self.active_buffer == 0 {
                self.buffers.len() - 1
            } else {
                self.active_buffer - 1
            };
            self.select_buffer(prev);
        }
    }

    pub fn jump_to_unread(&mut self) {
        let len = self.buffers.len();
        if len == 0 {
            return;
        }
        for offset in 1..=len {
            let idx = (self.active_buffer + offset) % len;
            if self.buffers[idx].unread {
                self.select_buffer(idx);
                return;
            }
        }
    }

    pub fn find_buffer(&self, server_id: &str, name: &str) -> Option<usize> {
        self.buffers.iter().position(|b| {
            b.server_id.eq_ignore_ascii_case(server_id) && b.name.eq_ignore_ascii_case(name)
        })
    }

    pub fn find_server_buffer(&self, server_id: &str) -> Option<usize> {
        self.buffers.iter().position(|b| {
            b.server_id.eq_ignore_ascii_case(server_id) && b.kind == BufferKind::Server
        })
    }

    /// Ensures a buffer for `(server_id, name, kind)` exists, inserting it at the end of
    /// `server_id`'s group in `self.buffers` so the tree remains grouped by server.
    pub fn ensure_buffer(&mut self, server_id: &str, name: &str, kind: BufferKind) -> usize {
        if let Some(idx) = self.find_buffer(server_id, name) {
            return idx;
        }

        let insert_pos = self
            .buffers
            .iter()
            .rposition(|b| b.server_id.eq_ignore_ascii_case(server_id))
            .map(|pos| pos + 1)
            .unwrap_or(self.buffers.len());

        let buf = Buffer::new(server_id.to_string(), name.to_string(), kind);
        self.buffers.insert(insert_pos, buf);

        if insert_pos <= self.active_buffer {
            self.active_buffer += 1;
        }

        insert_pos
    }

    pub fn push_to_buffer(&mut self, idx: usize, line: ChatLine) {
        let is_active = idx == self.active_buffer;
        if let Some(buf) = self.buffers.get_mut(idx) {
            buf.push_line(line, is_active);
        }
    }

    pub fn handle_key_event(&mut self, key: KeyEvent) {
        if self.show_help {
            self.handle_help_key_event(key);
            return;
        }

        if key.code != KeyCode::Tab {
            self.tab_state = None;
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);

        if ctrl {
            match key.code {
                KeyCode::Char('c') | KeyCode::Char('q') => {
                    self.quit_all(None);
                    return;
                }
                KeyCode::Char('n') => {
                    self.next_buffer();
                    return;
                }
                KeyCode::Char('p') => {
                    self.prev_buffer();
                    return;
                }
                KeyCode::Char('a') => {
                    self.cursor_pos = 0;
                    return;
                }
                KeyCode::Char('e') => {
                    self.cursor_pos = self.input.len();
                    return;
                }
                KeyCode::Char('u') => {
                    self.input.clear();
                    self.cursor_pos = 0;
                    return;
                }
                KeyCode::Char('w') => {
                    self.delete_word_backwards();
                    return;
                }
                _ => {}
            }
        }

        if alt {
            match key.code {
                KeyCode::Down => {
                    self.next_buffer();
                    return;
                }
                KeyCode::Up => {
                    self.prev_buffer();
                    return;
                }
                KeyCode::Char('a') | KeyCode::Char('A') => {
                    self.jump_to_unread();
                    return;
                }
                KeyCode::PageUp => {
                    let buf = self.active_buf_mut();
                    buf.user_scroll_offset = buf.user_scroll_offset.saturating_sub(5);
                    return;
                }
                KeyCode::PageDown => {
                    let buf = self.active_buf_mut();
                    if buf.user_scroll_offset + 5 < buf.users.len() {
                        buf.user_scroll_offset += 5;
                    }
                    return;
                }
                KeyCode::Char(c @ '1'..='9') => {
                    let idx = (c as usize) - ('1' as usize);
                    self.select_buffer(idx);
                    return;
                }
                _ => {}
            }
        }

        match key.code {
            KeyCode::Tab => self.complete_tab(),
            KeyCode::Enter => self.submit_input(),
            KeyCode::Up => self.history_prev(),
            KeyCode::Down => self.history_next(),
            KeyCode::Left => {
                if self.cursor_pos > 0 {
                    self.cursor_pos = prev_char_boundary(&self.input, self.cursor_pos);
                }
            }
            KeyCode::Right => {
                if self.cursor_pos < self.input.len() {
                    self.cursor_pos = next_char_boundary(&self.input, self.cursor_pos);
                }
            }
            KeyCode::Home => self.cursor_pos = 0,
            KeyCode::End => self.cursor_pos = self.input.len(),
            KeyCode::Backspace => {
                if self.cursor_pos > 0 {
                    let prev = prev_char_boundary(&self.input, self.cursor_pos);
                    self.input.replace_range(prev..self.cursor_pos, "");
                    self.cursor_pos = prev;
                }
            }
            KeyCode::Delete => {
                if self.cursor_pos < self.input.len() {
                    let next = next_char_boundary(&self.input, self.cursor_pos);
                    self.input.replace_range(self.cursor_pos..next, "");
                }
            }
            KeyCode::PageUp => {
                let buf = self.active_buf_mut();
                buf.scroll_offset = buf.scroll_offset.saturating_add(5);
            }
            KeyCode::PageDown => {
                let buf = self.active_buf_mut();
                buf.scroll_offset = buf.scroll_offset.saturating_sub(5);
            }
            KeyCode::Esc => {
                self.active_buf_mut().scroll_offset = 0;
            }
            KeyCode::Char(ch) => {
                self.input.insert(self.cursor_pos, ch);
                self.cursor_pos += ch.len_utf8();
            }
            _ => {}
        }
    }

    fn handle_help_key_event(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl {
            match key.code {
                KeyCode::Char('c') | KeyCode::Char('q') => {
                    self.quit_all(None);
                }
                KeyCode::Char('u') => {
                    self.help_scroll = self.help_scroll.saturating_sub(8);
                }
                KeyCode::Char('d') => {
                    self.help_scroll = self.help_scroll.saturating_add(8);
                }
                _ => {}
            }
            return;
        }

        match key.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => {
                self.show_help = false;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.help_scroll = self.help_scroll.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.help_scroll = self.help_scroll.saturating_add(1);
            }
            KeyCode::PageUp => {
                self.help_scroll = self.help_scroll.saturating_sub(8);
            }
            KeyCode::PageDown | KeyCode::Char(' ') => {
                self.help_scroll = self.help_scroll.saturating_add(8);
            }
            KeyCode::Home | KeyCode::Char('g') => {
                self.help_scroll = 0;
            }
            KeyCode::End | KeyCode::Char('G') => {
                self.help_scroll = 1000;
            }
            _ => {}
        }
    }

    pub fn delete_word_backwards(&mut self) {
        if self.cursor_pos == 0 {
            return;
        }
        let before = &self.input[..self.cursor_pos];
        let trimmed_ws = before.trim_end_matches(char::is_whitespace);
        let word_start = trimmed_ws
            .rfind(char::is_whitespace)
            .map(|i| i + 1)
            .unwrap_or(0);
        self.input.replace_range(word_start..self.cursor_pos, "");
        self.cursor_pos = word_start;
    }

    pub fn history_prev(&mut self) {
        if self.input_history.is_empty() {
            return;
        }
        let next_idx = match self.history_index {
            None => {
                self.saved_draft = self.input.clone();
                self.input_history.len() - 1
            }
            Some(0) => 0,
            Some(i) => i - 1,
        };
        self.history_index = Some(next_idx);
        self.input = self.input_history[next_idx].clone();
        self.cursor_pos = self.input.len();
    }

    pub fn history_next(&mut self) {
        let Some(idx) = self.history_index else {
            return;
        };
        if idx + 1 < self.input_history.len() {
            let next_idx = idx + 1;
            self.history_index = Some(next_idx);
            self.input = self.input_history[next_idx].clone();
        } else {
            self.history_index = None;
            self.input = std::mem::take(&mut self.saved_draft);
        }
        self.cursor_pos = self.input.len();
    }

    pub fn complete_tab(&mut self) {
        if let Some(mut state) = self.tab_state.take() {
            if !state.candidates.is_empty() {
                state.index = (state.index + 1) % state.candidates.len();
                let replacement = &state.candidates[state.index];
                let end = state.replace_start + state.replace_len;
                self.input
                    .replace_range(state.replace_start..end, replacement);
                state.replace_len = replacement.len();
                self.cursor_pos = state.replace_start + state.replace_len;
                self.tab_state = Some(state);
            }
            return;
        }

        let before_cursor = &self.input[..self.cursor_pos];
        let token_start = before_cursor
            .rfind(char::is_whitespace)
            .map(|i| i + 1)
            .unwrap_or(0);
        let token = &before_cursor[token_start..];
        if token.is_empty() {
            return;
        }

        let candidates: Vec<String> = if token_start == 0 && token.starts_with('/') {
            let lower = token.to_ascii_lowercase();
            SLASH_COMMANDS
                .iter()
                .filter(|cmd| cmd.starts_with(&lower))
                .map(|cmd| format!("{cmd} "))
                .collect()
        } else {
            let lower = token.to_ascii_lowercase();
            let suffix = if token_start == 0 { ": " } else { " " };
            self.active_buf()
                .users
                .iter()
                .map(|u| strip_user_prefix(u).1)
                .filter(|nick| nick.to_ascii_lowercase().starts_with(&lower))
                .map(|nick| format!("{nick}{suffix}"))
                .collect()
        };

        if candidates.is_empty() {
            return;
        }

        let replacement = candidates[0].clone();
        self.input
            .replace_range(token_start..self.cursor_pos, &replacement);
        let replace_len = replacement.len();
        self.cursor_pos = token_start + replace_len;
        self.tab_state = Some(TabCompletionState {
            replace_start: token_start,
            replace_len,
            candidates,
            index: 0,
        });
    }

    pub fn submit_input(&mut self) {
        let line = std::mem::take(&mut self.input);
        self.cursor_pos = 0;
        self.history_index = None;
        self.saved_draft.clear();

        if line.trim().is_empty() {
            return;
        }

        if self.input_history.last() != Some(&line) {
            self.input_history.push(line.clone());
        }

        let Some(cmd) = parse_user_command(&line) else {
            self.push_to_buffer(
                self.active_buffer,
                ChatLine::new(
                    None,
                    format!("Unknown or incomplete command: {}", line.trim()),
                    LineKind::Error,
                ),
            );
            return;
        };

        self.execute_user_command(cmd);
    }

    pub fn execute_user_command(&mut self, cmd: UserCommand) {
        let server_id = self.active_server_id();
        let active_nick = self.active_nick().to_string();

        match cmd {
            UserCommand::Help => {
                self.show_help = true;
                self.help_scroll = 0;
            }
            UserCommand::Join(channel) => {
                if let Some(sender) = self.senders.get(&server_id) {
                    let _ = sender.send_join(&channel);
                }
                let idx = self.ensure_buffer(&server_id, &channel, BufferKind::Channel);
                self.select_buffer(idx);
            }
            UserCommand::Part(reason) => {
                if self.active_buf().kind != BufferKind::Channel {
                    self.push_to_buffer(
                        self.active_buffer,
                        ChatLine::new(None, "Cannot /part a non-channel buffer", LineKind::Error),
                    );
                    return;
                }
                let chan = self.active_buf().name.clone();
                if let Some(sender) = self.senders.get(&server_id) {
                    let _ = sender.send(Command::PART(chan.clone(), reason));
                }
                self.push_to_buffer(
                    self.active_buffer,
                    ChatLine::new(None, format!("Left {chan}"), LineKind::System),
                );
            }
            UserCommand::Msg { target, text } => {
                if let Some(sender) = self.senders.get(&server_id) {
                    let _ = sender.send_privmsg(&target, &text);
                }
                let kind = if target.starts_with('#') || target.starts_with('&') {
                    BufferKind::Channel
                } else {
                    BufferKind::DirectMessage
                };
                let idx = self.ensure_buffer(&server_id, &target, kind);
                self.push_to_buffer(idx, ChatLine::new(Some(active_nick), text, LineKind::Normal));
                self.select_buffer(idx);
            }
            UserCommand::Nick(new_nick) => {
                if let Some(sender) = self.senders.get(&server_id) {
                    let _ = sender.send(Command::NICK(new_nick.clone()));
                }
                let old_nick = self
                    .nicks
                    .insert(server_id.clone(), new_nick.clone())
                    .unwrap_or_default();
                for buf in &mut self.buffers {
                    if buf.server_id.eq_ignore_ascii_case(&server_id) {
                        buf.rename_user(&old_nick, &new_nick);
                    }
                }
                self.push_to_buffer(
                    self.active_buffer,
                    ChatLine::new(
                        None,
                        format!("Nickname changed to {new_nick}"),
                        LineKind::System,
                    ),
                );
            }
            UserCommand::Me(action) => {
                if self.active_buf().kind == BufferKind::Server {
                    self.push_to_buffer(
                        self.active_buffer,
                        ChatLine::new(
                            None,
                            "Cannot send /me in a server buffer",
                            LineKind::Error,
                        ),
                    );
                    return;
                }
                let target = self.active_buf().name.clone();
                if let Some(sender) = self.senders.get(&server_id) {
                    let ctcp = format!("\x01ACTION {action}\x01");
                    let _ = sender.send_privmsg(&target, &ctcp);
                }
                self.push_to_buffer(
                    self.active_buffer,
                    ChatLine::new(Some(active_nick), action, LineKind::Action),
                );
            }
            UserCommand::Topic(new_topic) => {
                if self.active_buf().kind != BufferKind::Channel {
                    self.push_to_buffer(
                        self.active_buffer,
                        ChatLine::new(
                            None,
                            "Cannot use /topic outside a channel buffer",
                            LineKind::Error,
                        ),
                    );
                    return;
                }
                let chan = self.active_buf().name.clone();
                if new_topic.is_none() {
                    if let Some(existing) = self.active_buf().topic.clone() {
                        self.push_to_buffer(
                            self.active_buffer,
                            ChatLine::new(
                                None,
                                format!("* Topic for {chan}: {existing}"),
                                LineKind::System,
                            ),
                        );
                    }
                }
                if let Some(sender) = self.senders.get(&server_id) {
                    let _ = sender.send(Command::TOPIC(chan, new_topic));
                }
            }
            UserCommand::Whois(target) => {
                if let Some(sender) = self.senders.get(&server_id) {
                    let _ = sender.send(Command::WHOIS(None, target.clone()));
                }
                self.push_to_buffer(
                    self.active_buffer,
                    ChatLine::new(
                        None,
                        format!("* Querying WHOIS for {target}..."),
                        LineKind::System,
                    ),
                );
            }
            UserCommand::Close => {
                if self.active_buf().kind == BufferKind::Server {
                    self.push_to_buffer(
                        self.active_buffer,
                        ChatLine::new(None, "Cannot /close a server root buffer", LineKind::Error),
                    );
                    return;
                }
                if self.active_buf().kind == BufferKind::Channel {
                    let chan = self.active_buf().name.clone();
                    if let Some(sender) = self.senders.get(&server_id) {
                        let _ = sender.send(Command::PART(chan, None));
                    }
                }
                self.buffers.remove(self.active_buffer);
                if self.active_buffer >= self.buffers.len() && !self.buffers.is_empty() {
                    self.active_buffer = self.buffers.len() - 1;
                }
                if !self.buffers.is_empty() {
                    self.buffers[self.active_buffer].unread = false;
                }
            }
            UserCommand::Quit(reason) => {
                self.quit_all(reason);
            }
            UserCommand::Say(text) => {
                if self.active_buf().kind == BufferKind::Server {
                    self.push_to_buffer(
                        self.active_buffer,
                        ChatLine::new(
                            None,
                            "Cannot send plain chat to server buffer (use /join <#channel> or /msg <nick> <text>)",
                            LineKind::Error,
                        ),
                    );
                    return;
                }
                let target = self.active_buf().name.clone();
                if let Some(sender) = self.senders.get(&server_id) {
                    let _ = sender.send_privmsg(&target, &text);
                }
                self.push_to_buffer(
                    self.active_buffer,
                    ChatLine::new(Some(active_nick), text, LineKind::Normal),
                );
            }
        }
    }

    pub fn quit_all(&mut self, reason: Option<String>) {
        let quit_msg = reason.or_else(|| Some("ronf shutting down".to_string()));
        for sender in self.senders.values() {
            let _ = sender.send(Command::QUIT(quit_msg.clone()));
        }
        self.should_quit = true;
    }

    pub fn handle_server_event(&mut self, event: ServerEvent) {
        match event {
            ServerEvent::Connected { server_id, sender } => {
                self.senders.insert(server_id.clone(), sender);
                if let Some(idx) = self.find_server_buffer(&server_id) {
                    self.push_to_buffer(
                        idx,
                        ChatLine::new(
                            None,
                            format!("Connected to [{server_id}]"),
                            LineKind::System,
                        ),
                    );
                }
            }
            ServerEvent::Error { server_id, error } => {
                if let Some(idx) = self.find_server_buffer(&server_id) {
                    self.push_to_buffer(idx, ChatLine::new(None, error, LineKind::Error));
                }
            }
            ServerEvent::Message { server_id, message } => {
                self.handle_irc_message(&server_id, message);
            }
        }
    }

    pub fn handle_irc_message(&mut self, server_id: &str, msg: Message) {
        let sender_nick = extract_nick(&msg.prefix);
        let my_nick = self
            .nicks
            .get(server_id)
            .cloned()
            .unwrap_or_else(|| "irc_user".to_string());

        match msg.command {
            Command::PRIVMSG(target, text) => {
                let is_channel = target.starts_with('#') || target.starts_with('&');
                let (buf_name, buf_kind) = if is_channel {
                    (target, BufferKind::Channel)
                } else {
                    (
                        sender_nick.clone().unwrap_or(target),
                        BufferKind::DirectMessage,
                    )
                };

                let idx = self.ensure_buffer(server_id, &buf_name, buf_kind);

                if let Some(action) = strip_ctcp_action(&text) {
                    self.push_to_buffer(
                        idx,
                        ChatLine::new(sender_nick, action.to_string(), LineKind::Action),
                    );
                } else {
                    self.push_to_buffer(idx, ChatLine::new(sender_nick, text, LineKind::Normal));
                }
            }
            Command::NOTICE(target, text) => {
                let idx = if target.starts_with('#') || target.starts_with('&') {
                    self.ensure_buffer(server_id, &target, BufferKind::Channel)
                } else {
                    self.find_server_buffer(server_id).unwrap_or(0)
                };
                let prefix = sender_nick
                    .map(|n| format!("-{n}- {text}"))
                    .unwrap_or(text);
                self.push_to_buffer(idx, ChatLine::new(None, prefix, LineKind::System));
            }
            Command::JOIN(chan, _, _) => {
                let idx = self.ensure_buffer(server_id, &chan, BufferKind::Channel);
                if let Some(nick) = sender_nick {
                    self.buffers[idx].add_user(&nick);
                    self.push_to_buffer(
                        idx,
                        ChatLine::new(
                            None,
                            format!("--> {nick} has joined {chan}"),
                            LineKind::System,
                        ),
                    );
                }
            }
            Command::PART(chan, reason) => {
                if let Some(idx) = self.find_buffer(server_id, &chan) {
                    if let Some(nick) = sender_nick {
                        self.buffers[idx].remove_user(&nick);
                        let suffix = reason.map(|r| format!(" ({r})")).unwrap_or_default();
                        self.push_to_buffer(
                            idx,
                            ChatLine::new(
                                None,
                                format!("<-- {nick} has left {chan}{suffix}"),
                                LineKind::System,
                            ),
                        );
                    }
                }
            }
            Command::KICK(chan, kicked, reason) => {
                if let Some(idx) = self.find_buffer(server_id, &chan) {
                    self.buffers[idx].remove_user(&kicked);
                    let by = sender_nick.unwrap_or_else(|| "server".to_string());
                    let suffix = reason.map(|r| format!(" ({r})")).unwrap_or_default();
                    self.push_to_buffer(
                        idx,
                        ChatLine::new(
                            None,
                            format!("<-- {kicked} was kicked from {chan} by {by}{suffix}"),
                            LineKind::System,
                        ),
                    );
                }
            }
            Command::QUIT(reason) => {
                if let Some(nick) = sender_nick {
                    let suffix = reason.map(|r| format!(" ({r})")).unwrap_or_default();
                    let affected: Vec<usize> = self
                        .buffers
                        .iter_mut()
                        .enumerate()
                        .filter_map(|(i, buf)| {
                            if buf.server_id.eq_ignore_ascii_case(server_id)
                                && buf.remove_user(&nick)
                            {
                                Some(i)
                            } else {
                                None
                            }
                        })
                        .collect();
                    for idx in affected {
                        self.push_to_buffer(
                            idx,
                            ChatLine::new(
                                None,
                                format!("<-- {nick} has quit{suffix}"),
                                LineKind::System,
                            ),
                        );
                    }
                }
            }
            Command::NICK(new_nick) => {
                if let Some(old_nick) = sender_nick {
                    if old_nick.eq_ignore_ascii_case(&my_nick) {
                        self.nicks.insert(server_id.to_string(), new_nick.clone());
                    }
                    let affected: Vec<usize> = self
                        .buffers
                        .iter_mut()
                        .enumerate()
                        .filter_map(|(i, buf)| {
                            if buf.server_id.eq_ignore_ascii_case(server_id)
                                && buf.rename_user(&old_nick, &new_nick)
                            {
                                Some(i)
                            } else {
                                None
                            }
                        })
                        .collect();
                    for idx in affected {
                        self.push_to_buffer(
                            idx,
                            ChatLine::new(
                                None,
                                format!("* {old_nick} is now known as {new_nick}"),
                                LineKind::System,
                            ),
                        );
                    }
                }
            }
            Command::TOPIC(chan, topic) => {
                let idx = self.ensure_buffer(server_id, &chan, BufferKind::Channel);
                self.buffers[idx].topic = topic.clone();
                if let Some(t) = topic {
                    self.push_to_buffer(
                        idx,
                        ChatLine::new(
                            None,
                            format!("* Topic for {chan}: {t}"),
                            LineKind::System,
                        ),
                    );
                }
            }
            Command::Response(resp, args) => {
                self.handle_numeric_response(server_id, resp, args);
            }
            _ => {}
        }
    }

    fn handle_numeric_response(&mut self, server_id: &str, resp: Response, args: Vec<String>) {
        match resp {
            Response::RPL_NOTOPIC => {
                if args.len() >= 2 {
                    let chan = &args[1];
                    let idx = self.ensure_buffer(server_id, chan, BufferKind::Channel);
                    self.buffers[idx].topic = None;
                    self.push_to_buffer(
                        idx,
                        ChatLine::new(
                            None,
                            format!("* No topic is set for {chan}"),
                            LineKind::System,
                        ),
                    );
                }
            }
            Response::RPL_TOPIC => {
                if args.len() >= 3 {
                    let chan = &args[1];
                    let topic = args[2..].join(" ");
                    let idx = self.ensure_buffer(server_id, chan, BufferKind::Channel);
                    self.buffers[idx].topic = Some(topic.clone());
                    self.push_to_buffer(
                        idx,
                        ChatLine::new(
                            None,
                            format!("* Topic for {chan}: {topic}"),
                            LineKind::System,
                        ),
                    );
                }
            }
            Response::RPL_NAMREPLY => {
                // Format: [<me>, "="|"*"|"@", <channel>, "<space-separated names>"]
                if args.len() >= 4 {
                    let chan = &args[2];
                    let names_str = &args[3];
                    let idx = self.ensure_buffer(server_id, chan, BufferKind::Channel);
                    for name in names_str.split_whitespace() {
                        if !name.is_empty() {
                            self.buffers[idx].pending_names.push(name.to_string());
                        }
                    }
                }
            }
            Response::RPL_ENDOFNAMES => {
                // Format: [<me>, <channel>, "End of /NAMES list."]
                if args.len() >= 2 {
                    let chan = &args[1];
                    if let Some(idx) = self.find_buffer(server_id, chan) {
                        let pending = std::mem::take(&mut self.buffers[idx].pending_names);
                        if !pending.is_empty() {
                            self.buffers[idx].users = pending;
                            self.buffers[idx].sort_users();
                        }
                    }
                }
            }
            Response::RPL_WHOISUSER => {
                if args.len() >= 6 {
                    let nick = &args[1];
                    let user = &args[2];
                    let host = &args[3];
                    let real = args[5..].join(" ");
                    self.push_to_buffer(
                        self.active_buffer,
                        ChatLine::new(
                            None,
                            format!("* [whois] {nick} ({user}@{host}): {real}"),
                            LineKind::System,
                        ),
                    );
                }
            }
            Response::RPL_WHOISSERVER => {
                if args.len() >= 4 {
                    let nick = &args[1];
                    let srv = &args[2];
                    let info = args[3..].join(" ");
                    self.push_to_buffer(
                        self.active_buffer,
                        ChatLine::new(
                            None,
                            format!("* [whois] {nick} server: {srv} ({info})"),
                            LineKind::System,
                        ),
                    );
                }
            }
            Response::RPL_WHOISOPERATOR => {
                if args.len() >= 3 {
                    let nick = &args[1];
                    let info = args[2..].join(" ");
                    self.push_to_buffer(
                        self.active_buffer,
                        ChatLine::new(
                            None,
                            format!("* [whois] {nick} {info}"),
                            LineKind::System,
                        ),
                    );
                }
            }
            Response::RPL_WHOISIDLE => {
                if args.len() >= 3 {
                    let nick = &args[1];
                    let secs = &args[2];
                    self.push_to_buffer(
                        self.active_buffer,
                        ChatLine::new(
                            None,
                            format!("* [whois] {nick} idle: {secs}s"),
                            LineKind::System,
                        ),
                    );
                }
            }
            Response::RPL_WHOISCHANNELS => {
                if args.len() >= 3 {
                    let nick = &args[1];
                    let chans = args[2..].join(" ");
                    self.push_to_buffer(
                        self.active_buffer,
                        ChatLine::new(
                            None,
                            format!("* [whois] {nick} channels: {chans}"),
                            LineKind::System,
                        ),
                    );
                }
            }
            Response::RPL_ENDOFWHOIS => {
                if args.len() >= 2 {
                    let nick = &args[1];
                    self.push_to_buffer(
                        self.active_buffer,
                        ChatLine::new(
                            None,
                            format!("* [whois] {nick} End of WHOIS"),
                            LineKind::System,
                        ),
                    );
                }
            }
            Response::RPL_WELCOME
            | Response::RPL_YOURHOST
            | Response::RPL_CREATED
            | Response::RPL_MOTDSTART
            | Response::RPL_MOTD
            | Response::RPL_ENDOFMOTD => {
                if let Some(srv_idx) = self.find_server_buffer(server_id) {
                    let text = if args.len() > 1 {
                        args[1..].join(" ")
                    } else {
                        args.join(" ")
                    };
                    self.push_to_buffer(srv_idx, ChatLine::new(None, text, LineKind::System));
                }
            }
            _ if resp.is_error() => {
                let text = if args.len() > 1 {
                    args[1..].join(" ")
                } else {
                    args.join(" ")
                };
                self.push_to_buffer(
                    self.active_buffer,
                    ChatLine::new(None, format!("! {text}"), LineKind::Error),
                );
            }
            _ => {}
        }
    }
}

fn extract_nick(prefix: &Option<Prefix>) -> Option<String> {
    match prefix {
        Some(Prefix::Nickname(nick, _, _)) => Some(nick.clone()),
        Some(Prefix::ServerName(srv)) => Some(srv.clone()),
        None => None,
    }
}

fn strip_ctcp_action(text: &str) -> Option<&str> {
    let rest = text.strip_prefix("\x01ACTION ")?;
    Some(rest.strip_suffix('\x01').unwrap_or(rest))
}

fn prev_char_boundary(s: &str, byte_idx: usize) -> usize {
    s[..byte_idx]
        .char_indices()
        .next_back()
        .map(|(i, _)| i)
        .unwrap_or(0)
}

fn next_char_boundary(s: &str, byte_idx: usize) -> usize {
    let mut iter = s[byte_idx..].char_indices();
    iter.next();
    iter.next()
        .map(|(offset, _)| byte_idx + offset)
        .unwrap_or(s.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_app() -> App {
        let toml = r##"
            default_nickname = "irc_user"

            [servers.libera]
            server = "irc.libera.chat"
            channels = ["#linux", "#rust"]

            [servers.oftc]
            server = "irc.oftc.net"
            channels = ["#rust"]
        "##;
        let cfg = AppConfig::from_toml_str(toml).unwrap();
        App::from_config(&cfg)
    }

    #[test]
    fn parses_slash_commands_and_plain_text() {
        assert_eq!(parse_user_command("/help"), Some(UserCommand::Help));
        assert_eq!(
            parse_user_command("/whois alice"),
            Some(UserCommand::Whois("alice".to_string()))
        );
        assert_eq!(parse_user_command("/topic"), Some(UserCommand::Topic(None)));
        assert_eq!(
            parse_user_command("/topic Rust 2024 edition"),
            Some(UserCommand::Topic(Some("Rust 2024 edition".to_string())))
        );
        assert_eq!(
            parse_user_command("/join #rust"),
            Some(UserCommand::Join("#rust".to_string()))
        );
        assert_eq!(
            parse_user_command("/msg alice hello there"),
            Some(UserCommand::Msg {
                target: "alice".to_string(),
                text: "hello there".to_string()
            })
        );
        assert_eq!(
            parse_user_command("/quit bye"),
            Some(UserCommand::Quit(Some("bye".to_string())))
        );
        assert_eq!(
            parse_user_command("plain chat line"),
            Some(UserCommand::Say("plain chat line".to_string()))
        );
    }

    #[test]
    fn isolates_buffers_across_servers() {
        let mut app = test_app();
        let libera_rust = app.find_buffer("libera", "#rust").unwrap();
        let oftc_rust = app.find_buffer("oftc", "#rust").unwrap();
        assert_ne!(libera_rust, oftc_rust);

        let msg = Message {
            tags: None,
            prefix: Some(Prefix::Nickname(
                "alice".into(),
                "alice".into(),
                "host".into(),
            )),
            command: Command::PRIVMSG("#rust".into(), "hello libera".into()),
        };
        app.handle_irc_message("libera", msg);

        assert_eq!(app.buffers[libera_rust].messages.len(), 1);
        assert_eq!(app.buffers[oftc_rust].messages.len(), 0);
        assert!(app.buffers[libera_rust].unread);
    }

    #[test]
    fn populates_sorts_and_updates_channel_user_list() {
        let mut app = test_app();
        let rust_idx = app.find_buffer("libera", "#rust").unwrap();

        app.handle_irc_message(
            "libera",
            Message {
                tags: None,
                prefix: None,
                command: Command::Response(
                    Response::RPL_NAMREPLY,
                    vec![
                        "irc_user".into(),
                        "=".into(),
                        "#rust".into(),
                        "charlie +bob @ferris alice".into(),
                    ],
                ),
            },
        );
        app.handle_irc_message(
            "libera",
            Message {
                tags: None,
                prefix: None,
                command: Command::Response(
                    Response::RPL_ENDOFNAMES,
                    vec!["irc_user".into(), "#rust".into(), "End of /NAMES".into()],
                ),
            },
        );

        assert_eq!(
            app.buffers[rust_idx].users,
            vec!["@ferris", "+bob", "alice", "charlie"]
        );

        // NICK change preserves @ role prefix
        app.handle_irc_message(
            "libera",
            Message {
                tags: None,
                prefix: Some(Prefix::Nickname("ferris".into(), "f".into(), "h".into())),
                command: Command::NICK("ferris_bot".into()),
            },
        );
        assert_eq!(
            app.buffers[rust_idx].users,
            vec!["@ferris_bot", "+bob", "alice", "charlie"]
        );

        // PART removes user
        app.handle_irc_message(
            "libera",
            Message {
                tags: None,
                prefix: Some(Prefix::Nickname("alice".into(), "a".into(), "h".into())),
                command: Command::PART("#rust".into(), None),
            },
        );
        assert_eq!(
            app.buffers[rust_idx].users,
            vec!["@ferris_bot", "+bob", "charlie"]
        );
    }

    #[test]
    fn tab_completes_nicks_and_slash_commands() {
        let mut app = test_app();
        let rust_idx = app.find_buffer("libera", "#rust").unwrap();
        app.select_buffer(rust_idx);
        app.buffers[rust_idx].users = vec!["@ferris".into(), "alice".into(), "alex".into()];

        // Start of line nick completion -> adds ": " and cycles on second Tab
        app.input = "al".to_string();
        app.cursor_pos = 2;
        app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.input, "alice: ");
        app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.input, "alex: ");

        // Mid-line nick completion -> adds " "
        app.handle_key_event(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        app.input = "hello fe".to_string();
        app.cursor_pos = app.input.len();
        app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.input, "hello ferris ");

        // Slash command completion
        app.handle_key_event(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        app.input = "/j".to_string();
        app.cursor_pos = 2;
        app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.input, "/join ");
    }

    #[test]
    fn input_history_and_unread_jump_work() {
        let mut app = test_app();
        let rust_idx = app.find_buffer("libera", "#rust").unwrap();
        app.select_buffer(rust_idx);

        app.input = "first message".into();
        app.cursor_pos = app.input.len();
        app.submit_input();

        app.input = "second message".into();
        app.cursor_pos = app.input.len();
        app.submit_input();

        app.input = "draft".into();
        app.cursor_pos = app.input.len();
        app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.input, "second message");
        app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.input, "first message");
        app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.input, "second message");
        app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.input, "draft");

        // Mark #linux unread and press Alt+A
        let linux_idx = app.find_buffer("libera", "#linux").unwrap();
        app.buffers[linux_idx].unread = true;
        app.handle_key_event(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::ALT));
        assert_eq!(app.active_buffer, linux_idx);
        assert!(!app.buffers[linux_idx].unread);
    }

    #[test]
    fn help_command_opens_scrolls_and_closes_popup() {
        let mut app = test_app();
        assert!(!app.show_help);

        app.input = "/help".into();
        app.cursor_pos = app.input.len();
        app.submit_input();
        assert!(app.show_help);
        assert_eq!(app.help_scroll, 0);

        app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.help_scroll, 1);

        app.handle_key_event(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        assert_eq!(app.help_scroll, 9);

        app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.help_scroll, 8);

        app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!app.show_help);
    }
}
