// SPDX-License-Identifier: GPL-3.0-or-later

use chrono::Local;
use irc::proto::{ChannelMode, Mode};

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
            users: Vec::new(),
            pending_names: Vec::new(),
            user_scroll_offset: 0,
            messages: Vec::new(),
            history_cursor: None,
            scroll_offset: 0,
            unread: false,
            highlight: false,
            parted: false,
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
            let (prefixes, raw) = extract_user_prefixes(user);
            *user = format!("{prefixes}{raw}");
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
        before != self.users.len()
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
    name.starts_with(['#', '&'])
}

fn is_role_prefix(ch: char) -> bool {
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

    let text_lower = text.to_ascii_lowercase();
    let nick_lower = nick_clean.to_ascii_lowercase();
    let is_nick_char = |b: u8| {
        b.is_ascii_alphanumeric()
            || matches!(
                b,
                b'_' | b'-' | b'[' | b']' | b'{' | b'}' | b'\\' | b'|' | b'^' | b'`'
            )
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
