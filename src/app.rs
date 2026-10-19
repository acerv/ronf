// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::Local;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use irc::client::Sender;
use irc::proto::{ChannelMode, Command, Message, Mode, Prefix, Response};

use crate::config::AppConfig;

pub const SLASH_COMMANDS: &[&str] = &[
    "/help",
    "/join",
    "/part",
    "/msg",
    "/query",
    "/nick",
    "/me",
    "/topic",
    "/whois",
    "/kick",
    "/invite",
    "/mode",
    "/ban",
    "/op",
    "/deop",
    "/voice",
    "/devoice",
    "/away",
    "/motd",
    "/list",
    "/buffer",
    "/reconnect",
    "/ignore",
    "/unignore",
    "/search",
    "/raw",
    "/quote",
    "/clear",
    "/close",
    "/quit",
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
    pub highlight: bool,
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
            highlight: false,
        }
    }

    pub fn push_line(&mut self, line: ChatLine, is_active: bool) {
        self.messages.push(line);
        if !is_active {
            self.unread = true;
        }
    }

    pub fn sort_users(&mut self) {
        for user in &mut self.users {
            let (pref, raw) = strip_user_prefix(user);
            *user = match pref {
                Some(p) => format!("{p}{raw}"),
                None => raw.to_string(),
            };
        }
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

    pub fn apply_channel_modes(&mut self, modes: &[Mode<ChannelMode>]) {
        let mut changed = false;
        for mode in modes {
            match mode {
                Mode::Plus(cm, Some(target)) => {
                    if let Some(new_pref) = channel_mode_prefix(cm) {
                        let (_, target_raw) = strip_user_prefix(target);
                        if let Some(user) = self.users.iter_mut().find(|u| {
                            let (_, n) = strip_user_prefix(u);
                            n.eq_ignore_ascii_case(target_raw)
                        }) {
                            let (cur_pref, raw) = strip_user_prefix(user);
                            if cur_pref.is_none_or(|cur| role_rank(Some(new_pref)) <= role_rank(Some(cur))) {
                                *user = format!("{new_pref}{raw}");
                                changed = true;
                            }
                        }
                    }
                }
                Mode::Minus(cm, Some(target)) => {
                    if let Some(rem_pref) = channel_mode_prefix(cm) {
                        let (_, target_raw) = strip_user_prefix(target);
                        if let Some(user) = self.users.iter_mut().find(|u| {
                            let (_, n) = strip_user_prefix(u);
                            n.eq_ignore_ascii_case(target_raw)
                        }) {
                            let (cur_pref, raw) = strip_user_prefix(user);
                            if cur_pref == Some(rem_pref) {
                                *user = raw.to_string();
                                changed = true;
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        if changed {
            self.sort_users();
        }
    }
}

fn channel_mode_prefix(cm: &ChannelMode) -> Option<char> {
    match cm {
        ChannelMode::Founder => Some('~'),
        ChannelMode::Admin => Some('&'),
        ChannelMode::Oper => Some('@'),
        ChannelMode::Halfop => Some('%'),
        ChannelMode::Voice => Some('+'),
        _ => None,
    }
}

pub fn strip_user_prefix(entry: &str) -> (Option<char>, &str) {
    let mut best_prefix: Option<char> = None;
    let mut byte_offset = 0;

    for ch in entry.chars() {
        if matches!(ch, '~' | '&' | '@' | '%' | '+') {
            byte_offset += ch.len_utf8();
            if best_prefix.is_none_or(|cur| role_rank(Some(ch)) < role_rank(Some(cur))) {
                best_prefix = Some(ch);
            }
        } else {
            break;
        }
    }

    (best_prefix, &entry[byte_offset..])
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
    Query { target: String, text: Option<String> },
    Nick(String),
    Me(String),
    Topic(Option<String>),
    Whois(String),
    Kick { nick: String, reason: Option<String> },
    Invite { nick: String, channel: Option<String> },
    Mode { target: Option<String>, args: String },
    Away(Option<String>),
    Motd,
    List(Option<String>),
    Buffer(String),
    Reconnect,
    Ignore(Option<String>),
    Unignore(String),
    Search(String),
    Raw(String),
    Clear,
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
        "/msg" => {
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
        "/query" => {
            if rest.is_empty() {
                None
            } else if let Some((target, text)) = rest.split_once(char::is_whitespace) {
                let text = text.trim();
                Some(UserCommand::Query {
                    target: target.to_string(),
                    text: if text.is_empty() {
                        None
                    } else {
                        Some(text.to_string())
                    },
                })
            } else {
                Some(UserCommand::Query {
                    target: rest.to_string(),
                    text: None,
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
        "/kick" => {
            if rest.is_empty() {
                None
            } else if let Some((nick, reason)) = rest.split_once(char::is_whitespace) {
                let reason = reason.trim();
                Some(UserCommand::Kick {
                    nick: nick.to_string(),
                    reason: if reason.is_empty() {
                        None
                    } else {
                        Some(reason.to_string())
                    },
                })
            } else {
                Some(UserCommand::Kick {
                    nick: rest.to_string(),
                    reason: None,
                })
            }
        }
        "/invite" => {
            if rest.is_empty() {
                None
            } else if let Some((nick, chan)) = rest.split_once(char::is_whitespace) {
                let chan = chan.trim();
                Some(UserCommand::Invite {
                    nick: nick.to_string(),
                    channel: if chan.is_empty() {
                        None
                    } else {
                        Some(chan.to_string())
                    },
                })
            } else {
                Some(UserCommand::Invite {
                    nick: rest.to_string(),
                    channel: None,
                })
            }
        }
        "/mode" => {
            if rest.is_empty() {
                None
            } else {
                let mut parts = rest.splitn(2, char::is_whitespace);
                let first = parts.next().unwrap();
                let rem = parts.next().map(str::trim).unwrap_or("");
                if first.starts_with('#') || first.starts_with('&') {
                    Some(UserCommand::Mode {
                        target: Some(first.to_string()),
                        args: rem.to_string(),
                    })
                } else {
                    Some(UserCommand::Mode {
                        target: None,
                        args: rest.to_string(),
                    })
                }
            }
        }
        "/ban" => {
            let mask = rest.split_whitespace().next()?;
            Some(UserCommand::Mode {
                target: None,
                args: format!("+b {mask}"),
            })
        }
        "/op" => {
            let nick = rest.split_whitespace().next()?;
            Some(UserCommand::Mode {
                target: None,
                args: format!("+o {nick}"),
            })
        }
        "/deop" => {
            let nick = rest.split_whitespace().next()?;
            Some(UserCommand::Mode {
                target: None,
                args: format!("-o {nick}"),
            })
        }
        "/voice" => {
            let nick = rest.split_whitespace().next()?;
            Some(UserCommand::Mode {
                target: None,
                args: format!("+v {nick}"),
            })
        }
        "/devoice" => {
            let nick = rest.split_whitespace().next()?;
            Some(UserCommand::Mode {
                target: None,
                args: format!("-v {nick}"),
            })
        }
        "/away" => {
            let msg = if rest.is_empty() {
                None
            } else {
                Some(rest.to_string())
            };
            Some(UserCommand::Away(msg))
        }
        "/motd" => Some(UserCommand::Motd),
        "/list" => {
            let filter = if rest.is_empty() {
                None
            } else {
                Some(rest.to_string())
            };
            Some(UserCommand::List(filter))
        }
        "/buffer" | "/b" => {
            if rest.is_empty() {
                None
            } else {
                Some(UserCommand::Buffer(rest.to_string()))
            }
        }
        "/reconnect" => Some(UserCommand::Reconnect),
        "/ignore" => {
            let nick = rest.split_whitespace().next().map(str::to_string);
            Some(UserCommand::Ignore(nick))
        }
        "/unignore" => {
            let nick = rest.split_whitespace().next()?;
            Some(UserCommand::Unignore(nick.to_string()))
        }
        "/search" => {
            if rest.is_empty() {
                None
            } else {
                Some(UserCommand::Search(rest.to_string()))
            }
        }
        "/raw" | "/quote" => {
            if rest.is_empty() {
                None
            } else {
                Some(UserCommand::Raw(rest.to_string()))
            }
        }
        "/clear" => Some(UserCommand::Clear),
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
        message: Box<Message>,
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
    pub show_join_part: BTreeMap<String, bool>,
    pub ignored_nicks: BTreeSet<String>,
    pub senders: HashMap<String, Sender>,
    pub input: String,
    pub cursor_pos: usize,
    pub input_history: Vec<String>,
    pub history_index: Option<usize>,
    pub saved_draft: String,
    pub tab_state: Option<TabCompletionState>,
    pub log_dir: Option<PathBuf>,
    pub show_help: bool,
    pub help_scroll: usize,
    pub should_quit: bool,
}

impl App {
    pub fn from_config(config: &AppConfig) -> Self {
        let mut buffers = Vec::new();
        let mut nicks = BTreeMap::new();
        let mut show_join_part = BTreeMap::new();
        let log_dir = config.effective_log_dir();

        for (server_id, srv) in config.enabled_servers() {
            let nick = srv
                .effective_nickname(config.default_nickname.as_deref())
                .unwrap_or_else(|_| "irc_user".to_string());
            nicks.insert(server_id.clone(), nick);
            show_join_part.insert(
                server_id.clone(),
                srv.effective_show_join_part(config.show_join_part),
            );

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
                let mut chan_buf = Buffer::new(
                    server_id.clone(),
                    chan.clone(),
                    BufferKind::Channel,
                );
                if let Some(ref dir) = log_dir {
                    load_buffer_history(dir, &mut chan_buf);
                }
                buffers.push(chan_buf);
            }
        }

        Self {
            buffers,
            active_buffer: 0,
            nicks,
            show_join_part,
            ignored_nicks: BTreeSet::new(),
            senders: HashMap::new(),
            input: String::new(),
            cursor_pos: 0,
            input_history: Vec::new(),
            history_index: None,
            saved_draft: String::new(),
            tab_state: None,
            log_dir,
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
            self.buffers[index].highlight = false;
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

    pub fn join_server_channels(&self, server_id: &str) {
        let Some(sender) = self.senders.get(server_id) else {
            return;
        };
        for buf in &self.buffers {
            if buf.server_id.eq_ignore_ascii_case(server_id) && buf.kind == BufferKind::Channel {
                let _ = sender.send_join(&buf.name);
            }
        }
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

        let mut buf = Buffer::new(server_id.to_string(), name.to_string(), kind);
        if let Some(ref dir) = self.log_dir {
            load_buffer_history(dir, &mut buf);
        }
        self.buffers.insert(insert_pos, buf);

        if insert_pos <= self.active_buffer {
            self.active_buffer += 1;
        }

        insert_pos
    }

    pub fn push_to_buffer(&mut self, idx: usize, line: ChatLine) {
        let is_active = idx == self.active_buffer;
        if let Some(buf) = self.buffers.get_mut(idx) {
            if let Some(ref dir) = self.log_dir {
                if buf.kind != BufferKind::Server
                    && matches!(line.kind, LineKind::Normal | LineKind::Action)
                {
                    append_buffer_log(dir, &buf.server_id, &buf.name, &line);
                }
            }
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
            .char_indices()
            .rev()
            .find(|(_, c)| c.is_whitespace())
            .map(|(i, c)| i + c.len_utf8())
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
            .char_indices()
            .rev()
            .find(|(_, c)| c.is_whitespace())
            .map(|(i, c)| i + c.len_utf8())
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
            UserCommand::Query { target, text } => {
                let idx = self.ensure_buffer(&server_id, &target, BufferKind::DirectMessage);
                if let Some(msg_text) = text {
                    if let Some(sender) = self.senders.get(&server_id) {
                        let _ = sender.send_privmsg(&target, &msg_text);
                    }
                    self.push_to_buffer(
                        idx,
                        ChatLine::new(Some(active_nick), msg_text, LineKind::Normal),
                    );
                }
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
            UserCommand::Kick { nick, reason } => {
                if self.active_buf().kind != BufferKind::Channel {
                    self.push_to_buffer(
                        self.active_buffer,
                        ChatLine::new(
                            None,
                            "Cannot /kick outside a channel buffer",
                            LineKind::Error,
                        ),
                    );
                    return;
                }
                let chan = self.active_buf().name.clone();
                if let Some(sender) = self.senders.get(&server_id) {
                    let _ = sender.send(Command::KICK(chan, nick, reason));
                }
            }
            UserCommand::Invite { nick, channel } => {
                let target_chan = match channel {
                    Some(c) => c,
                    None if self.active_buf().kind == BufferKind::Channel => {
                        self.active_buf().name.clone()
                    }
                    None => {
                        self.push_to_buffer(
                            self.active_buffer,
                            ChatLine::new(
                                None,
                                "Specify a channel: /invite <nick> <#channel>",
                                LineKind::Error,
                            ),
                        );
                        return;
                    }
                };
                if let Some(sender) = self.senders.get(&server_id) {
                    let _ = sender.send(Command::INVITE(nick.clone(), target_chan.clone()));
                }
                self.push_to_buffer(
                    self.active_buffer,
                    ChatLine::new(
                        None,
                        format!("* Invited {nick} to {target_chan}"),
                        LineKind::System,
                    ),
                );
            }
            UserCommand::Mode { target, args } => {
                let target_name = match target {
                    Some(t) => t,
                    None if self.active_buf().kind == BufferKind::Channel => {
                        self.active_buf().name.clone()
                    }
                    None => active_nick,
                };
                if let Some(sender) = self.senders.get(&server_id) {
                    let mut raw_args = vec![target_name.clone()];
                    raw_args.extend(args.split_whitespace().map(str::to_string));
                    let _ = sender.send(Command::Raw("MODE".to_string(), raw_args));
                }
                self.push_to_buffer(
                    self.active_buffer,
                    ChatLine::new(
                        None,
                        format!("* Mode {target_name} {args}").trim_end().to_string(),
                        LineKind::System,
                    ),
                );
            }
            UserCommand::Away(reason) => {
                if let Some(sender) = self.senders.get(&server_id) {
                    let _ = sender.send(Command::AWAY(reason.clone()));
                }
                let text = match reason {
                    Some(r) => format!("* Marked as away: {r}"),
                    None => "* Cleared away status".to_string(),
                };
                self.push_to_buffer(
                    self.active_buffer,
                    ChatLine::new(None, text, LineKind::System),
                );
            }
            UserCommand::Motd => {
                if let Some(sender) = self.senders.get(&server_id) {
                    let _ = sender.send(Command::MOTD(None));
                }
            }
            UserCommand::List(filter) => {
                if let Some(sender) = self.senders.get(&server_id) {
                    let _ = sender.send(Command::LIST(filter, None));
                }
                self.push_to_buffer(
                    self.active_buffer,
                    ChatLine::new(None, "* Requesting channel list...", LineKind::System),
                );
            }
            UserCommand::Buffer(target) => {
                if let Ok(num) = target.parse::<usize>() {
                    if num >= 1 && num <= self.buffers.len() {
                        self.select_buffer(num - 1);
                        return;
                    }
                }
                if let Some(idx) = self
                    .buffers
                    .iter()
                    .position(|b| b.name.eq_ignore_ascii_case(&target))
                {
                    self.select_buffer(idx);
                } else {
                    self.push_to_buffer(
                        self.active_buffer,
                        ChatLine::new(
                            None,
                            format!("Buffer not found: {target}"),
                            LineKind::Error,
                        ),
                    );
                }
            }
            UserCommand::Reconnect => {
                if let Some(sender) = self.senders.remove(&server_id) {
                    let _ = sender.send(Command::QUIT(Some("Reconnecting...".to_string())));
                }
                self.push_to_buffer(
                    self.active_buffer,
                    ChatLine::new(
                        None,
                        format!("* Reconnecting to [{server_id}]..."),
                        LineKind::System,
                    ),
                );
            }
            UserCommand::Ignore(maybe_nick) => match maybe_nick {
                Some(nick) => {
                    self.ignored_nicks.insert(nick.to_ascii_lowercase());
                    self.push_to_buffer(
                        self.active_buffer,
                        ChatLine::new(None, format!("* Ignoring {nick}"), LineKind::System),
                    );
                }
                None => {
                    let msg = if self.ignored_nicks.is_empty() {
                        "* No users are currently ignored".to_string()
                    } else {
                        let list: Vec<&str> =
                            self.ignored_nicks.iter().map(String::as_str).collect();
                        format!("* Ignored users: {}", list.join(", "))
                    };
                    self.push_to_buffer(
                        self.active_buffer,
                        ChatLine::new(None, msg, LineKind::System),
                    );
                }
            },
            UserCommand::Unignore(nick) => {
                self.ignored_nicks.remove(&nick.to_ascii_lowercase());
                self.push_to_buffer(
                    self.active_buffer,
                    ChatLine::new(None, format!("* Unignored {nick}"), LineKind::System),
                );
            }
            UserCommand::Search(query) => {
                let q_lower = query.to_ascii_lowercase();
                let matches: Vec<String> = self
                    .active_buf()
                    .messages
                    .iter()
                    .filter(|m| !m.content.starts_with("* [search]"))
                    .filter(|m| {
                        m.content.to_ascii_lowercase().contains(&q_lower)
                            || m.sender
                                .as_deref()
                                .is_some_and(|s| s.to_ascii_lowercase().contains(&q_lower))
                    })
                    .map(|m| match &m.sender {
                        Some(s) => format!("* [search] [{}] <{s}> {}", m.timestamp, m.content),
                        None => format!("* [search] [{}] {}", m.timestamp, m.content),
                    })
                    .collect();

                if matches.is_empty() {
                    self.push_to_buffer(
                        self.active_buffer,
                        ChatLine::new(
                            None,
                            format!("* [search] No matches for '{query}'"),
                            LineKind::System,
                        ),
                    );
                } else {
                    for line in matches {
                        self.push_to_buffer(
                            self.active_buffer,
                            ChatLine::new(None, line, LineKind::System),
                        );
                    }
                }
            }
            UserCommand::Raw(raw_cmd) => {
                if let Some(sender) = self.senders.get(&server_id) {
                    if let Ok(msg) = format!("{raw_cmd}\r\n").parse::<Message>() {
                        let _ = sender.send(msg);
                    } else {
                        let mut parts = raw_cmd.split_whitespace();
                        if let Some(verb) = parts.next() {
                            let args: Vec<String> = parts.map(str::to_string).collect();
                            let _ = sender.send(Command::Raw(verb.to_string(), args));
                        }
                    }
                }
                self.push_to_buffer(
                    self.active_buffer,
                    ChatLine::new(None, format!("* >> {raw_cmd}"), LineKind::System),
                );
            }
            UserCommand::Clear => {
                let buf = self.active_buf_mut();
                buf.messages.clear();
                buf.scroll_offset = 0;
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
                    self.buffers[self.active_buffer].highlight = false;
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
                self.handle_irc_message(&server_id, *message);
            }
        }
    }

    pub fn handle_irc_message(&mut self, server_id: &str, msg: Message) {
        let sender_nick = extract_nick(&msg.prefix);
        let is_ignored = sender_nick
            .as_deref()
            .is_some_and(|n| self.ignored_nicks.contains(&n.to_ascii_lowercase()));
        let my_nick = self
            .nicks
            .get(server_id)
            .cloned()
            .unwrap_or_else(|| "irc_user".to_string());

        match msg.command {
            Command::PRIVMSG(_, _) | Command::NOTICE(_, _) if is_ignored => {}
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

                let is_new_dm = !is_channel && self.find_buffer(server_id, &buf_name).is_none();
                let idx = self.ensure_buffer(server_id, &buf_name, buf_kind);
                let is_from_self = sender_nick
                    .as_deref()
                    .map(|s| s.eq_ignore_ascii_case(&my_nick))
                    .unwrap_or(false);

                let display_text = strip_ctcp_action(&text).unwrap_or(&text);
                let is_mentioned = is_channel
                    && !is_from_self
                    && contains_nick_mention(display_text, &my_nick);

                if !is_from_self && (is_new_dm || !is_channel || is_mentioned) {
                    if idx != self.active_buffer {
                        self.buffers[idx].highlight = true;
                    }
                    if is_new_dm {
                        send_desktop_notification(
                            &format!("New conversation from {buf_name} ({server_id})"),
                            display_text,
                        );
                    } else if is_mentioned {
                        let from = sender_nick.as_deref().unwrap_or("someone");
                        send_desktop_notification(
                            &format!("Mentioned by {from} in {buf_name} ({server_id})"),
                            display_text,
                        );
                    } else if !is_channel && idx != self.active_buffer {
                        send_desktop_notification(
                            &format!("Message from {buf_name} ({server_id})"),
                            display_text,
                        );
                    }
                }

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
                    let should_show = self
                        .show_join_part
                        .get(server_id)
                        .copied()
                        .unwrap_or(true);
                    if should_show {
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
            }
            Command::PART(chan, reason) => {
                if let Some(idx) = self.find_buffer(server_id, &chan) {
                    if let Some(nick) = sender_nick {
                        self.buffers[idx].remove_user(&nick);
                        let should_show = self
                            .show_join_part
                            .get(server_id)
                            .copied()
                            .unwrap_or(true);
                        if should_show {
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
                    let should_show = self
                        .show_join_part
                        .get(server_id)
                        .copied()
                        .unwrap_or(true);
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
                    if should_show {
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
            Command::INVITE(invited, chan) => {
                let by = sender_nick.unwrap_or_else(|| "someone".to_string());
                self.push_to_buffer(
                    self.active_buffer,
                    ChatLine::new(
                        None,
                        format!("* {by} invited {invited} to {chan}"),
                        LineKind::System,
                    ),
                );
            }
            Command::ChannelMODE(chan, modes) => {
                if let Some(idx) = self.find_buffer(server_id, &chan) {
                    self.buffers[idx].apply_channel_modes(&modes);
                    let by = sender_nick.unwrap_or_else(|| "server".to_string());
                    let mode_strs: Vec<String> = modes.iter().map(|m| m.to_string()).collect();
                    self.push_to_buffer(
                        idx,
                        ChatLine::new(
                            None,
                            format!("* {by} sets mode {} on {chan}", mode_strs.join(" ")),
                            LineKind::System,
                        ),
                    );
                }
            }
            Command::Response(resp, args) => {
                self.handle_numeric_response(server_id, resp, args);
            }
            Command::Raw(code, args) => {
                self.handle_raw_numeric(code.as_str(), args);
            }
            _ => {}
        }
    }

    fn handle_raw_numeric(&mut self, code: &str, args: Vec<String>) {
        match code {
            "330" => {
                // RPL_WHOISACCOUNT: [<me>, <nick>, <account>, "is logged in as"]
                if args.len() >= 3 {
                    let nick = &args[1];
                    let account = &args[2];
                    self.push_to_buffer(
                        self.active_buffer,
                        ChatLine::new(
                            None,
                            format!("* [whois] {nick} is logged in as {account}"),
                            LineKind::System,
                        ),
                    );
                }
            }
            "378" | "671" => {
                // RPL_WHOISHOST (378) / RPL_WHOISSECURE (671): [<me>, <nick>, <info...>]
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
            Response::RPL_UNAWAY | Response::RPL_NOWAWAY => {
                let text = if args.len() > 1 {
                    args[1..].join(" ")
                } else {
                    args.join(" ")
                };
                self.push_to_buffer(
                    self.active_buffer,
                    ChatLine::new(None, format!("* {text}"), LineKind::System),
                );
            }
            Response::RPL_LIST => {
                if args.len() >= 3 {
                    let chan = &args[1];
                    let count = &args[2];
                    let topic = if args.len() >= 4 {
                        args[3..].join(" ")
                    } else {
                        String::new()
                    };
                    self.push_to_buffer(
                        self.active_buffer,
                        ChatLine::new(
                            None,
                            format!("* [list] {chan} ({count}): {topic}"),
                            LineKind::System,
                        ),
                    );
                }
            }
            Response::RPL_LISTEND => {
                self.push_to_buffer(
                    self.active_buffer,
                    ChatLine::new(None, "* [list] End of /LIST", LineKind::System),
                );
            }
            Response::ERR_NICKNAMEINUSE => {
                let current = self
                    .nicks
                    .get(server_id)
                    .cloned()
                    .unwrap_or_else(|| "irc_user".to_string());
                let fallback = format!("{current}_");
                self.nicks.insert(server_id.to_string(), fallback.clone());
                if let Some(sender) = self.senders.get(server_id) {
                    let _ = sender.send(Command::NICK(fallback.clone()));
                }
                let idx = self.find_server_buffer(server_id).unwrap_or(self.active_buffer);
                self.push_to_buffer(
                    idx,
                    ChatLine::new(
                        None,
                        format!("! Nickname '{current}' in use; switching to '{fallback}'"),
                        LineKind::Error,
                    ),
                );
            }
            Response::RPL_WELCOME
            | Response::RPL_YOURHOST
            | Response::RPL_CREATED
            | Response::RPL_MOTDSTART
            | Response::RPL_MOTD
            | Response::RPL_ENDOFMOTD
            | Response::ERR_NOMOTD => {
                if let Some(srv_idx) = self.find_server_buffer(server_id) {
                    let text = if args.len() > 1 {
                        args[1..].join(" ")
                    } else {
                        args.join(" ")
                    };
                    self.push_to_buffer(srv_idx, ChatLine::new(None, text, LineKind::System));
                }
                if matches!(resp, Response::RPL_ENDOFMOTD | Response::ERR_NOMOTD) {
                    self.join_server_channels(server_id);
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

pub fn contains_nick_mention(text: &str, nick: &str) -> bool {
    let nick_clean = strip_user_prefix(nick).1.trim();
    if nick_clean.is_empty() {
        return false;
    }

    let text_lower = text.to_ascii_lowercase();
    let nick_lower = nick_clean.to_ascii_lowercase();
    let is_nick_char = |b: u8| {
        b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'[' | b']' | b'{' | b'}' | b'\\' | b'|' | b'^' | b'`')
    };

    let text_bytes = text_lower.as_bytes();
    let nick_len = nick_lower.len();

    for (pos, _) in text_lower.match_indices(&nick_lower) {
        let before_ok = pos == 0 || !is_nick_char(text_bytes[pos - 1]);
        let after_pos = pos + nick_len;
        let after_ok = after_pos >= text_bytes.len() || !is_nick_char(text_bytes[after_pos]);
        if before_ok && after_ok {
            return true;
        }
    }

    false
}

#[cfg(not(test))]
fn send_desktop_notification(summary: &str, body: &str) {
    let _ = notify_rust::Notification::new()
        .appname("ronf")
        .summary(summary)
        .body(body)
        .icon("user-available")
        .show();
}

#[cfg(test)]
fn send_desktop_notification(_summary: &str, _body: &str) {}

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

fn buffer_log_path(base_dir: &Path, server_id: &str, buf_name: &str) -> PathBuf {
    let safe_srv: String = server_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let safe_buf: String = buf_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '#' | '&' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    base_dir.join(safe_srv).join(format!("{safe_buf}.log"))
}

fn append_buffer_log(base_dir: &Path, server_id: &str, buf_name: &str, line: &ChatLine) {
    let path = buffer_log_path(base_dir, server_id, buf_name);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        match (&line.kind, &line.sender) {
            (LineKind::Action, Some(sender)) => {
                let _ = writeln!(file, "[{}] * {} {}", line.timestamp, sender, line.content);
            }
            (_, Some(sender)) => {
                let _ = writeln!(file, "[{}] <{}> {}", line.timestamp, sender, line.content);
            }
            (_, None) => {
                let _ = writeln!(file, "[{}] {}", line.timestamp, line.content);
            }
        }
    }
}

fn load_buffer_history(base_dir: &Path, buf: &mut Buffer) {
    if buf.kind == BufferKind::Server {
        return;
    }
    let path = buffer_log_path(base_dir, &buf.server_id, &buf.name);
    let Ok(content) = fs::read_to_string(path) else {
        return;
    };
    let all_lines: Vec<&str> = content.lines().collect();
    let start = all_lines.len().saturating_sub(100);
    for raw in &all_lines[start..] {
        if let Some(rest) = raw.strip_prefix('[') {
            if let Some((ts, after_ts)) = rest.split_once("] ") {
                if let Some(action_rest) = after_ts.strip_prefix("* ") {
                    if let Some((actor, act_text)) = action_rest.split_once(' ') {
                        buf.messages.push(ChatLine {
                            timestamp: ts.to_string(),
                            sender: Some(actor.to_string()),
                            content: act_text.to_string(),
                            kind: LineKind::Action,
                        });
                        continue;
                    }
                } else if let Some(nick_rest) = after_ts.strip_prefix('<') {
                    if let Some((sender, msg_text)) = nick_rest.split_once("> ") {
                        buf.messages.push(ChatLine {
                            timestamp: ts.to_string(),
                            sender: Some(sender.to_string()),
                            content: msg_text.to_string(),
                            kind: LineKind::Normal,
                        });
                        continue;
                    }
                }
            }
        }
    }
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
            parse_user_command("/reconnect"),
            Some(UserCommand::Reconnect)
        );
        assert_eq!(
            parse_user_command("/away lunch"),
            Some(UserCommand::Away(Some("lunch".to_string())))
        );
        assert_eq!(parse_user_command("/motd"), Some(UserCommand::Motd));
        assert_eq!(
            parse_user_command("/buffer #rust"),
            Some(UserCommand::Buffer("#rust".to_string()))
        );
        assert_eq!(
            parse_user_command("/op alice"),
            Some(UserCommand::Mode {
                target: None,
                args: "+o alice".to_string()
            })
        );
        assert_eq!(
            parse_user_command("/ban *!*@bad.host"),
            Some(UserCommand::Mode {
                target: None,
                args: "+b *!*@bad.host".to_string()
            })
        );
        assert_eq!(
            parse_user_command("/kick bob spamming"),
            Some(UserCommand::Kick {
                nick: "bob".to_string(),
                reason: Some("spamming".to_string())
            })
        );
        assert_eq!(
            parse_user_command("/invite alice #rust"),
            Some(UserCommand::Invite {
                nick: "alice".to_string(),
                channel: Some("#rust".to_string())
            })
        );
        assert_eq!(
            parse_user_command("/raw PRIVMSG NickServ :IDENTIFY pass"),
            Some(UserCommand::Raw("PRIVMSG NickServ :IDENTIFY pass".to_string()))
        );
        assert_eq!(
            parse_user_command("/quote PING 123"),
            Some(UserCommand::Raw("PING 123".to_string()))
        );
        assert_eq!(parse_user_command("/clear"), Some(UserCommand::Clear));
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
            parse_user_command("/query alice"),
            Some(UserCommand::Query {
                target: "alice".to_string(),
                text: None
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
                        "charlie +bob @+ferris alice".into(),
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

        // ChannelMODE +o charlie promotes charlie to @charlie and re-sorts
        app.handle_irc_message(
            "libera",
            Message {
                tags: None,
                prefix: Some(Prefix::Nickname("ChanServ".into(), "c".into(), "h".into())),
                command: Command::ChannelMODE(
                    "#rust".into(),
                    vec![Mode::Plus(ChannelMode::Oper, Some("charlie".into()))],
                ),
            },
        );
        assert_eq!(
            app.buffers[rust_idx].users,
            vec!["@charlie", "@ferris_bot", "+bob"]
        );

        // ChannelMODE -o charlie demotes charlie back to regular user
        app.handle_irc_message(
            "libera",
            Message {
                tags: None,
                prefix: Some(Prefix::Nickname("ChanServ".into(), "c".into(), "h".into())),
                command: Command::ChannelMODE(
                    "#rust".into(),
                    vec![Mode::Minus(ChannelMode::Oper, Some("charlie".into()))],
                ),
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

        // Multi-byte Unicode whitespace (U+00A0 NO-BREAK SPACE) does not panic on Tab or Ctrl+W
        app.handle_key_event(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        app.input = "ab\u{00a0}al".to_string();
        app.cursor_pos = app.input.len();
        app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.input, "ab\u{00a0}alice ");
        app.handle_key_event(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(app.input, "ab\u{00a0}");
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

    #[test]
    fn detects_nick_mentions_and_new_dm_conversations() {
        assert!(contains_nick_mention("hey irc_user, how are you?", "irc_user"));
        assert!(contains_nick_mention("irc_user: ping", "irc_user"));
        assert!(!contains_nick_mention("other_irc_user_2 hello", "irc_user"));

        let mut app = test_app();
        let rust_idx = app.find_buffer("libera", "#rust").unwrap();

        // Mention in inactive #rust channel sets highlight = true
        app.handle_irc_message(
            "libera",
            Message {
                tags: None,
                prefix: Some(Prefix::Nickname("alice".into(), "a".into(), "h".into())),
                command: Command::PRIVMSG("#rust".into(), "ping irc_user!".into()),
            },
        );
        assert!(app.buffers[rust_idx].highlight);

        // Selecting #rust clears highlight
        app.select_buffer(rust_idx);
        assert!(!app.buffers[rust_idx].highlight);

        // Incoming new DM creates buffer and sets highlight = true
        app.handle_irc_message(
            "libera",
            Message {
                tags: None,
                prefix: Some(Prefix::Nickname("bob".into(), "b".into(), "h".into())),
                command: Command::PRIVMSG("irc_user".into(), "hey there!".into()),
            },
        );
        let dm_idx = app.find_buffer("libera", "bob").unwrap();
        assert_eq!(app.buffers[dm_idx].kind, BufferKind::DirectMessage);
        assert!(app.buffers[dm_idx].highlight);
    }

    #[test]
    fn appends_underscore_on_err_nicknameinuse() {
        let mut app = test_app();
        assert_eq!(app.nicks.get("libera").map(String::as_str), Some("irc_user"));

        app.handle_irc_message(
            "libera",
            Message {
                tags: None,
                prefix: None,
                command: Command::Response(
                    Response::ERR_NICKNAMEINUSE,
                    vec!["*".into(), "irc_user".into(), "Nickname is already in use.".into()],
                ),
            },
        );

        assert_eq!(app.nicks.get("libera").map(String::as_str), Some("irc_user_"));
    }

    #[test]
    fn handles_whois_account_host_and_tls_numerics() {
        let mut app = test_app();
        let active = app.active_buffer;
        let before = app.buffers[active].messages.len();

        app.handle_irc_message(
            "libera",
            Message {
                tags: None,
                prefix: None,
                command: Command::Raw(
                    "330".into(),
                    vec![
                        "irc_user".into(),
                        "alice".into(),
                        "alice_acct".into(),
                        "is logged in as".into(),
                    ],
                ),
            },
        );
        app.handle_irc_message(
            "libera",
            Message {
                tags: None,
                prefix: None,
                command: Command::Raw(
                    "671".into(),
                    vec![
                        "irc_user".into(),
                        "alice".into(),
                        "is using a secure connection".into(),
                    ],
                ),
            },
        );

        assert_eq!(app.buffers[active].messages.len(), before + 2);
        assert!(app.buffers[active].messages[before]
            .content
            .contains("is logged in as alice_acct"));
        assert!(app.buffers[active].messages[before + 1]
            .content
            .contains("is using a secure connection"));
    }

    #[test]
    fn ignore_and_unignore_filters_messages() {
        let mut app = test_app();
        let rust_idx = app.find_buffer("libera", "#rust").unwrap();

        app.execute_user_command(UserCommand::Ignore(Some("SpamBot".into())));
        app.handle_irc_message(
            "libera",
            Message {
                tags: None,
                prefix: Some(Prefix::Nickname("spambot".into(), "s".into(), "h".into())),
                command: Command::PRIVMSG("#rust".into(), "buy cheap stuff".into()),
            },
        );
        assert_eq!(app.buffers[rust_idx].messages.len(), 0);

        app.execute_user_command(UserCommand::Unignore("spambot".into()));
        app.handle_irc_message(
            "libera",
            Message {
                tags: None,
                prefix: Some(Prefix::Nickname("spambot".into(), "s".into(), "h".into())),
                command: Command::PRIVMSG("#rust".into(), "hello again".into()),
            },
        );
        assert_eq!(app.buffers[rust_idx].messages.len(), 1);
    }

    #[test]
    fn search_command_finds_matching_messages() {
        let mut app = test_app();
        let rust_idx = app.find_buffer("libera", "#rust").unwrap();
        app.select_buffer(rust_idx);

        app.handle_irc_message(
            "libera",
            Message {
                tags: None,
                prefix: Some(Prefix::Nickname("alice".into(), "a".into(), "h".into())),
                command: Command::PRIVMSG("#rust".into(), "borrow checker is great".into()),
            },
        );
        app.execute_user_command(UserCommand::Search("borrow".into()));
        assert_eq!(app.buffers[rust_idx].messages.len(), 2);
        assert!(app.buffers[rust_idx].messages[1]
            .content
            .contains("* [search]"));
    }

    #[test]
    fn persists_and_reloads_chat_logs() {
        let tmp_dir = std::env::temp_dir().join(format!("ronf_test_logs_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp_dir);

        let toml = format!(
            r##"
            default_nickname = "irc_user"
            enable_logging = true
            log_dir = "{}"

            [servers.libera]
            server = "irc.libera.chat"
            channels = ["#rust"]
        "##,
            tmp_dir.display()
        );

        let cfg = AppConfig::from_toml_str(&toml).unwrap();
        let mut app = App::from_config(&cfg);
        app.handle_irc_message(
            "libera",
            Message {
                tags: None,
                prefix: Some(Prefix::Nickname("alice".into(), "a".into(), "h".into())),
                command: Command::PRIVMSG("#rust".into(), "logged message".into()),
            },
        );

        // Reload new App instance from same config and verify history is loaded
        let app2 = App::from_config(&cfg);
        let rust_idx = app2.find_buffer("libera", "#rust").unwrap();
        assert_eq!(app2.buffers[rust_idx].messages.len(), 1);
        assert_eq!(app2.buffers[rust_idx].messages[0].content, "logged message");

        let _ = fs::remove_dir_all(&tmp_dir);
    }

    #[test]
    fn suppresses_join_part_quit_when_disabled_in_config() {
        let toml = r##"
            default_nickname = "irc_user"
            show_join_part = false

            [servers.libera]
            server = "irc.libera.chat"
            channels = ["#rust"]
        "##;
        let cfg = AppConfig::from_toml_str(toml).unwrap();
        let mut app = App::from_config(&cfg);
        let rust_idx = app.find_buffer("libera", "#rust").unwrap();

        // JOIN adds user to users list, but doesn't post system message
        app.handle_irc_message(
            "libera",
            Message {
                tags: None,
                prefix: Some(Prefix::Nickname("alice".into(), "a".into(), "h".into())),
                command: Command::JOIN("#rust".into(), None, None),
            },
        );
        assert_eq!(app.buffers[rust_idx].users, vec!["alice"]);
        assert_eq!(app.buffers[rust_idx].messages.len(), 0);

        // PART removes user, but doesn't post system message
        app.handle_irc_message(
            "libera",
            Message {
                tags: None,
                prefix: Some(Prefix::Nickname("alice".into(), "a".into(), "h".into())),
                command: Command::PART("#rust".into(), None),
            },
        );
        assert_eq!(app.buffers[rust_idx].users.len(), 0);
        assert_eq!(app.buffers[rust_idx].messages.len(), 0);

        // QUIT removes user, but doesn't post system message
        app.buffers[rust_idx].add_user("bob");
        app.handle_irc_message(
            "libera",
            Message {
                tags: None,
                prefix: Some(Prefix::Nickname("bob".into(), "b".into(), "h".into())),
                command: Command::QUIT(None),
            },
        );
        assert_eq!(app.buffers[rust_idx].users.len(), 0);
        assert_eq!(app.buffers[rust_idx].messages.len(), 0);
    }
}
