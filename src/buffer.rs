// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::VecDeque;

use chrono::{Local, Timelike};
use irc::proto::{ChannelMode, Mode};

pub const MAX_BUFFER_MESSAGES: usize = 5000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timestamp([u8; 8]);

impl Timestamp {
    pub fn now() -> Self {
        let now = Local::now();
        let h = now.hour() as u8;
        let m = now.minute() as u8;
        let s = now.second() as u8;
        Self([
            b'0' + h / 10,
            b'0' + h % 10,
            b':',
            b'0' + m / 10,
            b'0' + m % 10,
            b':',
            b'0' + s / 10,
            b'0' + s % 10,
        ])
    }

    pub fn from_hms_str(s: &str) -> Self {
        if let Ok(bytes) = <[u8; 8]>::try_from(s.as_bytes()) {
            if bytes.is_ascii() {
                return Self(bytes);
            }
        }
        Self(*b"??:??:??")
    }

    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).unwrap_or("??:??:??")
    }
}

impl std::fmt::Display for Timestamp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Normal,
    Action,
    System,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatLine {
    pub timestamp: Timestamp,
    pub sender: Option<String>,
    pub content: String,
    pub kind: LineKind,
}

impl ChatLine {
    pub fn new(sender: Option<String>, content: impl Into<String>, kind: LineKind) -> Self {
        Self {
            timestamp: Timestamp::now(),
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
    pub channel_key: Option<String>,
    pub users: Vec<String>,
    pub pending_names: Vec<String>,
    pub user_scroll_offset: usize,
    pub messages: VecDeque<ChatLine>,
    pub history_cursor: Option<usize>,
    pub scroll_offset: usize,
    pub unread: bool,
    pub highlight: bool,
    pub parted: bool,
}

impl Buffer {
    pub fn new(server_id: impl Into<String>, name: impl Into<String>, kind: BufferKind) -> Self {
        Self {
            server_id: server_id.into(),
            name: name.into(),
            kind,
            topic: None,
            channel_key: None,
            users: Vec::new(),
            pending_names: Vec::new(),
            user_scroll_offset: 0,
            messages: VecDeque::new(),
            history_cursor: None,
            scroll_offset: 0,
            unread: false,
            highlight: false,
            parted: false,
        }
    }

    pub fn push_line(&mut self, line: ChatLine, is_active: bool) {
        self.messages.push_back(line);
        if self.scroll_offset > 0 {
            self.scroll_offset = self.scroll_offset.saturating_add(1);
        } else if self.messages.len() > MAX_BUFFER_MESSAGES {
            self.messages.pop_front();
        }
        if !is_active || self.scroll_offset > 0 {
            self.unread = true;
        }
    }

    pub fn mark_read(&mut self) {
        self.unread = false;
        self.highlight = false;
    }

    pub fn mark_parted(&mut self) {
        self.parted = true;
        self.users.clear();
        self.pending_names.clear();
        self.user_scroll_offset = 0;
    }

    pub fn clamp_user_scroll(&mut self) {
        if self.users.is_empty() {
            self.user_scroll_offset = 0;
        } else if self.user_scroll_offset >= self.users.len() {
            self.user_scroll_offset = self.users.len() - 1;
        }
    }

    pub fn sort_users(&mut self) {
        for user in &mut self.users {
            let raw = user.trim_start_matches(is_role_prefix);
            let prefix_len = user.len() - raw.len();
            if prefix_len > 1 {
                let (prefixes, raw) = extract_user_prefixes(user);
                *user = format!("{prefixes}{raw}");
            }
        }
        self.users.retain(|u| !strip_user_prefix(u).1.is_empty());
        self.users.sort_by(|a, b| {
            let (pref_a, nick_a) = strip_user_prefix(a);
            let (pref_b, nick_b) = strip_user_prefix(b);
            cmp_ascii_ignore_case(nick_a, nick_b).then_with(|| role_rank(pref_a).cmp(&role_rank(pref_b)))
        });
        self.users
            .dedup_by(|a, b| strip_user_prefix(a).1.eq_ignore_ascii_case(strip_user_prefix(b).1));
        self.users.sort_by(|a, b| {
            let (pref_a, nick_a) = strip_user_prefix(a);
            let (pref_b, nick_b) = strip_user_prefix(b);
            role_rank(pref_a)
                .cmp(&role_rank(pref_b))
                .then_with(|| cmp_ascii_ignore_case(nick_a, nick_b))
        });
        self.clamp_user_scroll();
    }

    fn find_user_mut(&mut self, nick: &str) -> Option<&mut String> {
        let (_, target) = strip_user_prefix(nick);
        self.users
            .iter_mut()
            .find(|u| strip_user_prefix(u).1.eq_ignore_ascii_case(target))
    }

    pub fn add_user(&mut self, entry: &str) {
        let (_, new_nick) = strip_user_prefix(entry);
        if new_nick.is_empty() {
            return;
        }
        if let Some(existing) = self.find_user_mut(new_nick) {
            *existing = entry.to_string();
        } else {
            self.users.push(entry.to_string());
        }
        self.sort_users();
    }

    pub fn remove_user(&mut self, nick: &str) -> bool {
        let (_, target_nick) = strip_user_prefix(nick);
        let before = self.users.len();
        self.users
            .retain(|u| !strip_user_prefix(u).1.eq_ignore_ascii_case(target_nick));
        let changed = before != self.users.len();
        if changed {
            self.clamp_user_scroll();
        }
        changed
    }

    pub fn rename_user(&mut self, old_nick: &str, new_nick: &str) -> bool {
        let (_, old_raw) = strip_user_prefix(old_nick);
        let (_, new_raw) = strip_user_prefix(new_nick);
        let mut found = false;
        for user in &mut self.users {
            let (prefixes, n) = extract_user_prefixes(user);
            if n.eq_ignore_ascii_case(old_raw) {
                *user = format!("{prefixes}{new_raw}");
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
            let (adding, cm, target) = match mode {
                Mode::Plus(cm, Some(target)) => (true, cm, target),
                Mode::Minus(cm, Some(target)) => (false, cm, target),
                _ => continue,
            };
            let Some(pref) = channel_mode_prefix(cm) else {
                continue;
            };
            let Some(user) = self.find_user_mut(target) else {
                continue;
            };
            let (mut prefixes, raw) = extract_user_prefixes(user);
            if adding && !prefixes.contains(pref) {
                prefixes.push(pref);
                *user = format!("{prefixes}{raw}");
                changed = true;
            } else if !adding && prefixes.contains(pref) {
                prefixes.retain(|c| c != pref);
                *user = format!("{prefixes}{raw}");
                changed = true;
            }
        }
        if changed {
            self.sort_users();
        }
    }
}

pub fn is_channel_name(name: &str) -> bool {
    name.starts_with(['#', '&', '!', '+']) && name.len() > 1
}

fn is_nick_special(b: u8) -> bool {
    matches!(
        b,
        b'_' | b'[' | b']' | b'{' | b'}' | b'\\' | b'|' | b'^' | b'`'
    )
}

fn is_nick_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || is_nick_special(b) || b == b'-'
}

fn cmp_ascii_ignore_case(a: &str, b: &str) -> std::cmp::Ordering {
    a.bytes()
        .map(|c| c.to_ascii_lowercase())
        .cmp(b.bytes().map(|c| c.to_ascii_lowercase()))
}

pub fn is_valid_nick(nick: &str) -> bool {
    let mut bytes = nick.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || is_nick_special(first)) {
        return false;
    }
    bytes.all(is_nick_byte)
}

pub fn is_role_prefix(ch: char) -> bool {
    matches!(ch, '~' | '&' | '@' | '%' | '+')
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

pub fn extract_user_prefixes(entry: &str) -> (String, &str) {
    let raw = entry.trim_start_matches(is_role_prefix);
    let mut prefs: Vec<char> = entry[..entry.len() - raw.len()].chars().collect();
    prefs.sort_by_key(|&c| role_rank(Some(c)));
    prefs.dedup();
    (prefs.into_iter().collect(), raw)
}

pub fn strip_user_prefix(entry: &str) -> (Option<char>, &str) {
    let raw = entry.trim_start_matches(is_role_prefix);
    let top = entry[..entry.len() - raw.len()]
        .chars()
        .min_by_key(|&c| role_rank(Some(c)));
    (top, raw)
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

pub fn contains_nick_mention(text: &str, nick: &str) -> bool {
    let nick_clean = strip_user_prefix(nick).1.trim();
    if nick_clean.is_empty() {
        return false;
    }

    let text_bytes = text.as_bytes();
    let nick_bytes = nick_clean.as_bytes();
    let nick_len = nick_bytes.len();
    if text_bytes.len() < nick_len {
        return false;
    }

    for (pos, window) in text_bytes.windows(nick_len).enumerate() {
        if !window.eq_ignore_ascii_case(nick_bytes) {
            continue;
        }
        let before_ok = pos == 0 || !is_nick_byte(text_bytes[pos - 1]);
        let after_pos = pos + nick_len;
        let after_ok = after_pos >= text_bytes.len() || !is_nick_byte(text_bytes[after_pos]);
        if before_ok && after_ok {
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sort_users_deduplicates_non_adjacent_roles_keeping_highest_rank() {
        let mut buf = Buffer::new("libera", "#rust", BufferKind::Channel);
        buf.users = vec![
            "bob".into(),
            "+charlie".into(),
            "@BOB".into(),
            "charlie".into(),
            "@alice".into(),
            "+alice".into(),
        ];
        buf.sort_users();
        assert_eq!(buf.users, vec!["@alice", "@BOB", "+charlie"]);
    }

    #[test]
    fn bounds_in_memory_messages_when_not_scrolled_up() {
        let mut buf = Buffer::new("libera", "#rust", BufferKind::Channel);
        for i in 0..(MAX_BUFFER_MESSAGES + 10) {
            buf.push_line(ChatLine::new(None, format!("line {i}"), LineKind::Normal), true);
        }
        assert_eq!(buf.messages.len(), MAX_BUFFER_MESSAGES);
        assert_eq!(buf.messages.front().unwrap().content, "line 10");
        assert_eq!(
            buf.messages.back().unwrap().content,
            format!("line {}", MAX_BUFFER_MESSAGES + 9)
        );
    }
}
