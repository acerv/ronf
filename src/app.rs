// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use irc::client::Sender;
use irc::proto::{Command, Message};

use crate::buffer::{is_channel_name, Buffer, BufferKind, ChatLine, LineKind};
use crate::command::{parse_user_command, UserCommand};
use crate::config::{AppConfig, DEFAULT_GENERIC_NICK, DEFAULT_HISTORY_LIMIT};
use crate::input::InputState;
use crate::log_store::{append_buffer_log, discover_dm_buffers, load_more_buffer_history};

#[derive(Debug, Clone)]
pub struct ServerState {
    pub nick: String,
    pub show_join_part: bool,
    pub history_limit: usize,
    pub sender: Option<Sender>,
}

pub struct App {
    pub buffers: Vec<Buffer>,
    pub active_buffer: usize,
    pub servers: HashMap<String, ServerState>,
    pub ignored_nicks: BTreeSet<String>,
    pub input: InputState,
    pub log_dir: Option<PathBuf>,
    pub show_help: bool,
    pub help_scroll: usize,
    pub should_quit: bool,
}

impl App {
    pub fn from_config(config: &AppConfig) -> Self {
        let mut buffers = Vec::new();
        let mut servers = HashMap::new();
        let log_dir = config.effective_log_dir();

        for (server_id, srv) in config.enabled_servers() {
            let nick = srv
                .effective_nickname(config.default_nickname.as_deref())
                .unwrap_or_else(|_| DEFAULT_GENERIC_NICK.to_string());
            let limit = srv.effective_history_limit(config.history_limit);
            servers.insert(
                server_id.clone(),
                ServerState {
                    nick,
                    show_join_part: srv.effective_show_join_part(config.show_join_part),
                    history_limit: limit,
                    sender: None,
                },
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
                    load_more_buffer_history(dir, &mut chan_buf, limit);
                }
                buffers.push(chan_buf);
            }

            if let Some(ref dir) = log_dir {
                let dm_buffers = discover_dm_buffers(dir, server_id, &buffers, limit);
                buffers.extend(dm_buffers);
            }
        }

        Self {
            buffers,
            active_buffer: 0,
            servers,
            ignored_nicks: BTreeSet::new(),
            input: InputState::default(),
            log_dir,
            show_help: false,
            help_scroll: 0,
            should_quit: false,
        }
    }

    pub fn history_limit_for(&self, server_id: &str) -> usize {
        self.servers
            .get(server_id)
            .map(|s| s.history_limit)
            .unwrap_or(DEFAULT_HISTORY_LIMIT)
    }

    pub fn should_show_join_part(&self, server_id: &str) -> bool {
        self.servers
            .get(server_id)
            .map(|s| s.show_join_part)
            .unwrap_or(true)
    }

    pub fn sender_for(&self, server_id: &str) -> Option<&Sender> {
        self.servers.get(server_id).and_then(|s| s.sender.as_ref())
    }

    pub(crate) fn set_nick(&mut self, server_id: &str, new_nick: String) -> String {
        if let Some(srv) = self.servers.get_mut(server_id) {
            std::mem::replace(&mut srv.nick, new_nick)
        } else {
            self.servers.insert(
                server_id.to_string(),
                ServerState {
                    nick: new_nick,
                    show_join_part: true,
                    history_limit: DEFAULT_HISTORY_LIMIT,
                    sender: None,
                },
            );
            String::new()
        }
    }

    pub(crate) fn set_sender(&mut self, server_id: &str, sender: Option<Sender>) -> Option<Sender> {
        if let Some(srv) = self.servers.get_mut(server_id) {
            std::mem::replace(&mut srv.sender, sender)
        } else {
            if let Some(s) = sender {
                self.servers.insert(
                    server_id.to_string(),
                    ServerState {
                        nick: DEFAULT_GENERIC_NICK.to_string(),
                        show_join_part: true,
                        history_limit: DEFAULT_HISTORY_LIMIT,
                        sender: Some(s),
                    },
                );
            }
            None
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

    pub fn nick_for(&self, server_id: &str) -> &str {
        self.servers
            .get(server_id)
            .map(|s| s.nick.as_str())
            .unwrap_or(DEFAULT_GENERIC_NICK)
    }

    pub fn active_nick(&self) -> &str {
        self.nick_for(&self.active_buf().server_id)
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

    pub fn target_buffer_for_server(&self, server_id: &str) -> usize {
        if self.active_buf().server_id.eq_ignore_ascii_case(server_id) {
            self.active_buffer
        } else {
            self.find_server_buffer(server_id)
                .unwrap_or(self.active_buffer)
        }
    }

    pub fn join_server_channels(&self, server_id: &str) {
        let Some(sender) = self.sender_for(server_id) else {
            return;
        };
        for buf in &self.buffers {
            if buf.server_id.eq_ignore_ascii_case(server_id)
                && buf.kind == BufferKind::Channel
                && !buf.parted
            {
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
        let limit = self.history_limit_for(server_id);
        if let Some(ref dir) = self.log_dir {
            load_more_buffer_history(dir, &mut buf, limit);
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

    pub fn push_system(&mut self, idx: usize, msg: impl Into<String>) {
        self.push_to_buffer(idx, ChatLine::new(None, msg, LineKind::System));
    }

    pub fn push_error(&mut self, idx: usize, msg: impl Into<String>) {
        self.push_to_buffer(idx, ChatLine::new(None, msg, LineKind::Error));
    }

    fn require_active_channel(&mut self, err_msg: &str) -> Option<String> {
        if self.active_buf().kind != BufferKind::Channel {
            self.push_error(self.active_buffer, err_msg);
            return None;
        }
        Some(self.active_buf().name.clone())
    }

    fn require_chat_target(&mut self, server_buf_err: &str) -> Option<String> {
        if self.active_buf().kind == BufferKind::Server {
            self.push_error(self.active_buffer, server_buf_err);
            return None;
        }
        if self.active_buf().parted {
            self.push_error(
                self.active_buffer,
                "Cannot send to parted channel (use /join to rejoin)",
            );
            return None;
        }
        Some(self.active_buf().name.clone())
    }

    fn send_irc(&mut self, server_id: &str, msg: impl Into<Message>) -> bool {
        if let Some(sender) = self.sender_for(server_id) {
            if sender.send(msg).is_ok() {
                return true;
            }
            self.set_sender(server_id, None);
        }
        self.push_error(
            self.active_buffer,
            format!("Not connected to [{server_id}]"),
        );
        false
    }

    fn send_privmsg_and_echo(
        &mut self,
        server_id: &str,
        buf_idx: usize,
        target: &str,
        wire_override: Option<&str>,
        display_text: String,
        kind: LineKind,
    ) -> bool {
        let wire = wire_override.unwrap_or(&display_text);
        if let Some(sender) = self.sender_for(server_id) {
            if sender.send_privmsg(target, wire).is_ok() {
                let nick = self.nick_for(server_id).to_string();
                self.push_to_buffer(buf_idx, ChatLine::new(Some(nick), display_text, kind));
                return true;
            }
            self.set_sender(server_id, None);
        }
        self.push_error(
            self.active_buffer,
            format!("Not connected to [{server_id}]; message not sent"),
        );
        false
    }

    pub fn handle_key_event(&mut self, key: KeyEvent) {
        if self.show_help {
            self.handle_help_key_event(key);
            return;
        }

        if key.code != KeyCode::Tab {
            self.input.tab_state = None;
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);

        if ctrl {
            match key.code {
                KeyCode::Char('c') => {
                    self.input.cancel();
                    return;
                }
                KeyCode::Char('q') => {
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
                    self.input.move_home();
                    return;
                }
                KeyCode::Char('e') => {
                    self.input.move_end();
                    return;
                }
                KeyCode::Char('u') => {
                    self.input.clear_line();
                    return;
                }
                KeyCode::Char('w') => {
                    self.input.delete_word_backwards();
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
            KeyCode::Tab => {
                let users = &self.buffers[self.active_buffer].users;
                self.input.complete_tab(users);
            }
            KeyCode::Enter => self.submit_input(),
            KeyCode::Up => self.input.history_prev(),
            KeyCode::Down => self.input.history_next(),
            KeyCode::Left => self.input.move_left(),
            KeyCode::Right => self.input.move_right(),
            KeyCode::Home => self.input.move_home(),
            KeyCode::End => self.input.move_end(),
            KeyCode::Backspace => self.input.backspace(),
            KeyCode::Delete => self.input.delete_forward(),
            KeyCode::PageUp => {
                let server_id = self.active_server_id();
                let limit = self.history_limit_for(&server_id);
                let log_dir = self.log_dir.clone();
                let buf = self.active_buf_mut();
                if buf.scroll_offset.saturating_add(5) >= buf.messages.len().saturating_sub(1) {
                    if let Some(ref dir) = log_dir {
                        load_more_buffer_history(dir, buf, limit);
                    }
                }
                let max_scroll = buf.messages.len().saturating_sub(1);
                buf.scroll_offset = buf.scroll_offset.saturating_add(5).min(max_scroll);
            }
            KeyCode::PageDown => {
                let buf = self.active_buf_mut();
                buf.scroll_offset = buf.scroll_offset.saturating_sub(5);
            }
            KeyCode::Esc => {
                self.active_buf_mut().scroll_offset = 0;
            }
            KeyCode::Char(ch) => self.input.insert_char(ch),
            _ => {}
        }
    }

    fn handle_help_key_event(&mut self, key: KeyEvent) {
        let max_scroll = crate::ui::help_line_count().saturating_sub(1);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl {
            match key.code {
                KeyCode::Char('c') | KeyCode::Char('q') => self.quit_all(None),
                KeyCode::Char('u') => self.help_scroll = self.help_scroll.saturating_sub(8),
                KeyCode::Char('d') => {
                    self.help_scroll = self.help_scroll.saturating_add(8).min(max_scroll);
                }
                _ => {}
            }
            return;
        }

        match key.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => self.show_help = false,
            KeyCode::Up | KeyCode::Char('k') => {
                self.help_scroll = self.help_scroll.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.help_scroll = self.help_scroll.saturating_add(1).min(max_scroll);
            }
            KeyCode::PageUp => self.help_scroll = self.help_scroll.saturating_sub(8),
            KeyCode::PageDown | KeyCode::Char(' ') => {
                self.help_scroll = self.help_scroll.saturating_add(8).min(max_scroll);
            }
            KeyCode::Home | KeyCode::Char('g') => self.help_scroll = 0,
            KeyCode::End | KeyCode::Char('G') => self.help_scroll = max_scroll,
            _ => {}
        }
    }

    pub fn submit_input(&mut self) {
        let Some(line) = self.input.take_submitted() else {
            return;
        };

        let Some(cmd) = parse_user_command(&line) else {
            self.push_error(
                self.active_buffer,
                format!("Unknown or incomplete command: {}", line.trim()),
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
                if !self.send_irc(&server_id, Command::JOIN(channel.clone(), None, None)) {
                    return;
                }
                let idx = self.ensure_buffer(&server_id, &channel, BufferKind::Channel);
                self.buffers[idx].parted = false;
                self.select_buffer(idx);
            }
            UserCommand::Part(reason) => {
                let Some(chan) = self.require_active_channel("Cannot /part a non-channel buffer")
                else {
                    return;
                };
                if !self.send_irc(&server_id, Command::PART(chan.clone(), reason)) {
                    return;
                }
                self.active_buf_mut().mark_parted();
                self.push_system(self.active_buffer, format!("Left {chan}"));
            }
            UserCommand::Msg { target, text } => {
                if self.sender_for(&server_id).is_none() {
                    self.push_error(
                        self.active_buffer,
                        format!("Not connected to [{server_id}]; message not sent"),
                    );
                    return;
                }
                let kind = if is_channel_name(&target) {
                    BufferKind::Channel
                } else {
                    BufferKind::DirectMessage
                };
                let idx = self.ensure_buffer(&server_id, &target, kind);
                if self.send_privmsg_and_echo(
                    &server_id,
                    idx,
                    &target,
                    None,
                    text,
                    LineKind::Normal,
                ) {
                    self.select_buffer(idx);
                }
            }
            UserCommand::Query { target, text } => {
                let idx = self.ensure_buffer(&server_id, &target, BufferKind::DirectMessage);
                if let Some(msg_text) = text {
                    if !self.send_privmsg_and_echo(
                        &server_id,
                        idx,
                        &target,
                        None,
                        msg_text,
                        LineKind::Normal,
                    ) {
                        return;
                    }
                }
                self.select_buffer(idx);
            }
            UserCommand::Nick(new_nick) => {
                self.send_irc(&server_id, Command::NICK(new_nick));
            }
            UserCommand::Me(action) => {
                let Some(target) = self.require_chat_target("Cannot send /me in a server buffer")
                else {
                    return;
                };
                let ctcp = format!("\x01ACTION {action}\x01");
                self.send_privmsg_and_echo(
                    &server_id,
                    self.active_buffer,
                    &target,
                    Some(&ctcp),
                    action,
                    LineKind::Action,
                );
            }
            UserCommand::Topic(new_topic) => {
                let Some(chan) =
                    self.require_active_channel("Cannot use /topic outside a channel buffer")
                else {
                    return;
                };
                if new_topic.is_none() {
                    if let Some(existing) = self.active_buf().topic.clone() {
                        self.push_system(
                            self.active_buffer,
                            format!("* Topic for {chan}: {existing}"),
                        );
                        if let Some(sender) = self.sender_for(&server_id) {
                            let _ = sender.send(Command::TOPIC(chan, None));
                        }
                        return;
                    }
                }
                self.send_irc(&server_id, Command::TOPIC(chan, new_topic));
            }
            UserCommand::Whois(target) => {
                if self.send_irc(&server_id, Command::WHOIS(None, target.clone())) {
                    self.push_system(
                        self.active_buffer,
                        format!("* Querying WHOIS for {target}..."),
                    );
                }
            }
            UserCommand::Kick { nick, reason } => {
                let Some(chan) =
                    self.require_active_channel("Cannot /kick outside a channel buffer")
                else {
                    return;
                };
                self.send_irc(&server_id, Command::KICK(chan, nick, reason));
            }
            UserCommand::Invite { nick, channel } => {
                let target_chan = match channel {
                    Some(c) => c,
                    None if self.active_buf().kind == BufferKind::Channel => {
                        self.active_buf().name.clone()
                    }
                    None => {
                        self.push_error(
                            self.active_buffer,
                            "Specify a channel: /invite <nick> <#channel>",
                        );
                        return;
                    }
                };
                if self.send_irc(
                    &server_id,
                    Command::INVITE(nick.clone(), target_chan.clone()),
                ) {
                    self.push_system(
                        self.active_buffer,
                        format!("* Invited {nick} to {target_chan}"),
                    );
                }
            }
            UserCommand::Mode {
                target,
                args,
                channel_only,
            } => {
                if channel_only
                    && target.is_none()
                    && self
                        .require_active_channel(
                            "Cannot use channel mode command outside a channel buffer",
                        )
                        .is_none()
                {
                    return;
                }
                let target_name = match target {
                    Some(t) => t,
                    None if self.active_buf().kind == BufferKind::Channel => {
                        self.active_buf().name.clone()
                    }
                    None => active_nick,
                };
                let mut raw_args = vec![target_name.clone()];
                raw_args.extend(args.split_whitespace().map(str::to_string));
                if self.send_irc(&server_id, Command::Raw("MODE".to_string(), raw_args)) {
                    self.push_system(
                        self.active_buffer,
                        format!("* Mode {target_name} {args}").trim_end().to_string(),
                    );
                }
            }
            UserCommand::Away(reason) => {
                if self.send_irc(&server_id, Command::AWAY(reason.clone())) {
                    let text = match reason {
                        Some(r) => format!("* Marked as away: {r}"),
                        None => "* Cleared away status".to_string(),
                    };
                    self.push_system(self.active_buffer, text);
                }
            }
            UserCommand::Motd => {
                self.send_irc(&server_id, Command::MOTD(None));
            }
            UserCommand::List(filter) => {
                if self.send_irc(&server_id, Command::LIST(filter, None)) {
                    self.push_system(self.active_buffer, "* Requesting channel list...");
                }
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
                    self.push_error(self.active_buffer, format!("Buffer not found: {target}"));
                }
            }
            UserCommand::Reconnect => {
                if let Some(sender) = self.set_sender(&server_id, None) {
                    let _ = sender.send(Command::QUIT(Some("Reconnecting...".to_string())));
                }
                self.reset_server_channels(&server_id);
                self.push_system(self.active_buffer, format!("* Reconnecting to [{server_id}]..."));
            }
            UserCommand::Ignore(maybe_nick) => match maybe_nick {
                Some(nick) => {
                    self.ignored_nicks.insert(nick.to_ascii_lowercase());
                    self.push_system(self.active_buffer, format!("* Ignoring {nick}"));
                }
                None => {
                    let msg = if self.ignored_nicks.is_empty() {
                        "* No users are currently ignored".to_string()
                    } else {
                        let list: Vec<&str> =
                            self.ignored_nicks.iter().map(String::as_str).collect();
                        format!("* Ignored users: {}", list.join(", "))
                    };
                    self.push_system(self.active_buffer, msg);
                }
            },
            UserCommand::Unignore(nick) => {
                self.ignored_nicks.remove(&nick.to_ascii_lowercase());
                self.push_system(self.active_buffer, format!("* Unignored {nick}"));
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
                    self.push_system(
                        self.active_buffer,
                        format!("* [search] No matches for '{query}'"),
                    );
                } else {
                    for line in matches {
                        self.push_system(self.active_buffer, line);
                    }
                }
            }
            UserCommand::Raw(raw_cmd) => {
                let msg = format!("{raw_cmd}\r\n")
                    .parse::<Message>()
                    .unwrap_or_else(|_| {
                        let mut parts = raw_cmd.split_whitespace();
                        let verb = parts.next().unwrap_or_default().to_string();
                        let args: Vec<String> = parts.map(str::to_string).collect();
                        Command::Raw(verb, args).into()
                    });
                if self.send_irc(&server_id, msg) {
                    self.push_system(self.active_buffer, format!("* >> {raw_cmd}"));
                }
            }
            UserCommand::Clear => {
                let buf = self.active_buf_mut();
                buf.messages.clear();
                buf.history_cursor = Some(0);
                buf.scroll_offset = 0;
            }
            UserCommand::Close => {
                if self.active_buf().kind == BufferKind::Server {
                    self.push_error(self.active_buffer, "Cannot /close a server root buffer");
                    return;
                }
                if self.active_buf().kind == BufferKind::Channel {
                    let chan = self.active_buf().name.clone();
                    if let Some(sender) = self.sender_for(&server_id) {
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
                let Some(target) = self.require_chat_target(
                    "Cannot send plain chat to server buffer (use /join <#channel> or /msg <nick> <text>)",
                ) else {
                    return;
                };
                self.send_privmsg_and_echo(
                    &server_id,
                    self.active_buffer,
                    &target,
                    None,
                    text,
                    LineKind::Normal,
                );
            }
        }
    }

    pub fn quit_all(&mut self, reason: Option<String>) {
        let quit_msg = reason.or_else(|| Some("ronf shutting down".to_string()));
        for srv in self.servers.values() {
            if let Some(sender) = &srv.sender {
                let _ = sender.send(Command::QUIT(quit_msg.clone()));
            }
        }
        self.should_quit = true;
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use irc::proto::{ChannelMode, Mode, Prefix, Response};

    use super::*;
    use crate::buffer::contains_nick_mention;

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
            vec!["@+ferris", "+bob", "alice", "charlie"]
        );

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
            vec!["@+ferris_bot", "+bob", "alice", "charlie"]
        );

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
            vec!["@+ferris_bot", "+bob", "charlie"]
        );

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
            vec!["@charlie", "@+ferris_bot", "+bob"]
        );

        app.handle_irc_message(
            "libera",
            Message {
                tags: None,
                prefix: Some(Prefix::Nickname("ChanServ".into(), "c".into(), "h".into())),
                command: Command::ChannelMODE(
                    "#rust".into(),
                    vec![Mode::Minus(ChannelMode::Oper, Some("ferris_bot".into()))],
                ),
            },
        );
        assert_eq!(
            app.buffers[rust_idx].users,
            vec!["@charlie", "+bob", "+ferris_bot"]
        );
    }

    #[test]
    fn tab_completes_nicks_and_slash_commands() {
        let mut app = test_app();
        let rust_idx = app.find_buffer("libera", "#rust").unwrap();
        app.select_buffer(rust_idx);
        app.buffers[rust_idx].users = vec!["@ferris".into(), "alice".into(), "alex".into()];

        app.input.set_text("al");
        app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.input.text, "alice: ");
        app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.input.text, "alex: ");

        app.handle_key_event(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        app.input.set_text("hello fe");
        app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.input.text, "hello ferris ");

        app.handle_key_event(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        app.input.set_text("/j");
        app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.input.text, "/join ");

        app.handle_key_event(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        app.input.set_text("ab\u{00a0}al");
        app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.input.text, "ab\u{00a0}alice ");
        app.handle_key_event(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(app.input.text, "ab\u{00a0}");
    }

    #[test]
    fn input_history_and_unread_jump_work() {
        let mut app = test_app();
        let rust_idx = app.find_buffer("libera", "#rust").unwrap();
        app.select_buffer(rust_idx);

        app.input.set_text("first message");
        app.submit_input();

        app.input.set_text("second message");
        app.submit_input();

        app.input.set_text("draft");
        app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.input.text, "second message");
        app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.input.text, "first message");
        app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.input.text, "second message");
        app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.input.text, "draft");

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

        app.input.set_text("/help");
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

        app.handle_irc_message(
            "libera",
            Message {
                tags: None,
                prefix: Some(Prefix::Nickname("alice".into(), "a".into(), "h".into())),
                command: Command::PRIVMSG("#rust".into(), "ping irc_user!".into()),
            },
        );
        assert!(app.buffers[rust_idx].highlight);

        app.select_buffer(rust_idx);
        assert!(!app.buffers[rust_idx].highlight);

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
        assert_eq!(app.nick_for("libera"), "irc_user");

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

        assert_eq!(app.nick_for("libera"), "irc_user_");
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

        let app2 = App::from_config(&cfg);
        let rust_idx = app2.find_buffer("libera", "#rust").unwrap();
        assert_eq!(app2.buffers[rust_idx].messages.len(), 1);
        assert_eq!(app2.buffers[rust_idx].messages[0].content, "logged message");

        let _ = fs::remove_dir_all(&tmp_dir);
    }

    #[test]
    fn history_limit_startup_dynamic_pageup_and_dm_restore_work() {
        let tmp_dir =
            std::env::temp_dir().join(format!("ronf_test_hist_limit_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp_dir);

        let toml = format!(
            r##"
            default_nickname = "irc_user"
            log_dir = "{}"
            history_limit = 2

            [servers.libera]
            server = "irc.libera.chat"
            channels = ["#rust"]
        "##,
            tmp_dir.display()
        );

        let cfg = AppConfig::from_toml_str(&toml).unwrap();
        let mut app = App::from_config(&cfg);

        for i in 1..=5 {
            app.handle_irc_message(
                "libera",
                Message {
                    tags: None,
                    prefix: Some(Prefix::Nickname("alice".into(), "a".into(), "h".into())),
                    command: Command::PRIVMSG("#rust".into(), format!("msg {i}")),
                },
            );
        }
        for i in 1..=2 {
            app.handle_irc_message(
                "libera",
                Message {
                    tags: None,
                    prefix: Some(Prefix::Nickname("bob".into(), "b".into(), "h".into())),
                    command: Command::PRIVMSG("irc_user".into(), format!("dm {i}")),
                },
            );
        }

        let mut app2 = App::from_config(&cfg);
        let rust_idx = app2.find_buffer("libera", "#rust").unwrap();
        assert_eq!(
            app2.buffers[rust_idx]
                .messages
                .iter()
                .map(|m| m.content.as_str())
                .collect::<Vec<_>>(),
            vec!["msg 4", "msg 5"]
        );

        let bob_idx = app2.find_buffer("libera", "bob").expect("bob DM restored");
        assert_eq!(app2.buffers[bob_idx].kind, BufferKind::DirectMessage);
        assert!(!app2.buffers[bob_idx].unread);
        assert!(!app2.buffers[bob_idx].highlight);
        assert_eq!(
            app2.buffers[bob_idx]
                .messages
                .iter()
                .map(|m| m.content.as_str())
                .collect::<Vec<_>>(),
            vec!["dm 1", "dm 2"]
        );

        app2.handle_irc_message(
            "libera",
            Message {
                tags: None,
                prefix: Some(Prefix::Nickname("alice".into(), "a".into(), "h".into())),
                command: Command::PRIVMSG("#rust".into(), "msg 6".into()),
            },
        );
        app2.select_buffer(rust_idx);
        assert_eq!(
            app2.buffers[rust_idx]
                .messages
                .iter()
                .map(|m| m.content.as_str())
                .collect::<Vec<_>>(),
            vec!["msg 4", "msg 5", "msg 6"]
        );

        app2.handle_key_event(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
        assert_eq!(
            app2.buffers[rust_idx]
                .messages
                .iter()
                .map(|m| m.content.as_str())
                .collect::<Vec<_>>(),
            vec!["msg 2", "msg 3", "msg 4", "msg 5", "msg 6"]
        );
        assert_eq!(app2.buffers[rust_idx].scroll_offset, 4);

        app2.handle_key_event(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
        assert_eq!(
            app2.buffers[rust_idx]
                .messages
                .iter()
                .map(|m| m.content.as_str())
                .collect::<Vec<_>>(),
            vec!["msg 1", "msg 2", "msg 3", "msg 4", "msg 5", "msg 6"]
        );
        assert_eq!(app2.buffers[rust_idx].history_cursor, Some(0));
        assert_eq!(app2.buffers[rust_idx].scroll_offset, 5);

        let zero_toml = format!(
            r##"
            default_nickname = "irc_user"
            log_dir = "{}"
            history_limit = 0

            [servers.libera]
            server = "irc.libera.chat"
            channels = ["#rust"]
        "##,
            tmp_dir.display()
        );
        let zero_cfg = AppConfig::from_toml_str(&zero_toml).unwrap();
        let app_zero = App::from_config(&zero_cfg);
        let zero_rust = app_zero.find_buffer("libera", "#rust").unwrap();
        assert!(app_zero.buffers[zero_rust].messages.is_empty());
        assert!(app_zero.find_buffer("libera", "bob").is_none());

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

    #[test]
    fn rejects_outbound_commands_while_disconnected_and_defers_nick_update() {
        let mut app = test_app();
        let rust_idx = app.find_buffer("libera", "#rust").unwrap();
        app.select_buffer(rust_idx);

        app.execute_user_command(UserCommand::Say("hello".into()));
        assert_eq!(app.buffers[rust_idx].messages.len(), 1);
        assert_eq!(app.buffers[rust_idx].messages[0].kind, LineKind::Error);

        app.execute_user_command(UserCommand::Msg {
            target: "bob".into(),
            text: "hi".into(),
        });
        assert!(app.find_buffer("libera", "bob").is_none());

        app.execute_user_command(UserCommand::Nick("new_nick".into()));
        assert_eq!(app.nick_for("libera"), "irc_user");
    }

    #[test]
    fn marks_channel_parted_and_clears_users_on_own_part_or_kick() {
        let mut app = test_app();
        let rust_idx = app.find_buffer("libera", "#rust").unwrap();
        let linux_idx = app.find_buffer("libera", "#linux").unwrap();

        app.buffers[rust_idx].users = vec!["alice".into(), "irc_user".into()];
        app.handle_irc_message(
            "libera",
            Message {
                tags: None,
                prefix: Some(Prefix::Nickname("irc_user".into(), "u".into(), "h".into())),
                command: Command::PART("#rust".into(), None),
            },
        );
        assert!(app.buffers[rust_idx].parted);
        assert!(app.buffers[rust_idx].users.is_empty());

        app.buffers[linux_idx].users = vec!["@op".into(), "irc_user".into()];
        app.handle_irc_message(
            "libera",
            Message {
                tags: None,
                prefix: Some(Prefix::Nickname("op".into(), "o".into(), "h".into())),
                command: Command::KICK("#linux".into(), "irc_user".into(), None),
            },
        );
        assert!(app.buffers[linux_idx].parted);
        assert!(app.buffers[linux_idx].users.is_empty());
    }

    #[test]
    fn routes_self_sent_dm_echo_to_recipient_buffer() {
        let mut app = test_app();
        app.handle_irc_message(
            "libera",
            Message {
                tags: None,
                prefix: Some(Prefix::Nickname("irc_user".into(), "u".into(), "h".into())),
                command: Command::PRIVMSG("bob".into(), "hello bob".into()),
            },
        );

        assert!(app.find_buffer("libera", "irc_user").is_none());
        let bob_idx = app.find_buffer("libera", "bob").unwrap();
        assert_eq!(app.buffers[bob_idx].kind, BufferKind::DirectMessage);
        assert_eq!(app.buffers[bob_idx].messages[0].content, "hello bob");
    }

    #[test]
    fn clears_stale_channel_users_and_topic_on_disconnect() {
        let mut app = test_app();
        let rust_idx = app.find_buffer("libera", "#rust").unwrap();
        let oftc_idx = app.find_buffer("oftc", "#rust").unwrap();

        app.buffers[rust_idx].users = vec!["@alice".into(), "bob".into()];
        app.buffers[rust_idx].topic = Some("old topic".into());
        app.buffers[oftc_idx].users = vec!["charlie".into()];

        app.handle_server_event(crate::irc_handler::ServerEvent::Error {
            server_id: "libera".into(),
            error: "Disconnected".into(),
        });

        assert!(app.buffers[rust_idx].users.is_empty());
        assert!(app.buffers[rust_idx].topic.is_none());
        assert_eq!(app.buffers[oftc_idx].users, vec!["charlie"]);
    }
}
