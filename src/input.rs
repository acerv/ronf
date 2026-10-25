// SPDX-License-Identifier: GPL-3.0-or-later

use crate::buffer::strip_user_prefix;
use crate::command::slash_command_names;

#[derive(Debug, Clone)]
pub struct TabCompletionState {
    pub replace_start: usize,
    pub replace_len: usize,
    pub candidates: Vec<String>,
    pub index: usize,
}

#[derive(Debug, Default, Clone)]
pub struct InputState {
    pub text: String,
    pub cursor_pos: usize,
    pub history: Vec<String>,
    pub history_index: Option<usize>,
    pub saved_draft: String,
    pub tab_state: Option<TabCompletionState>,
}

impl InputState {
    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cursor_pos = self.text.len();
    }

    pub fn cancel(&mut self) {
        self.text.clear();
        self.cursor_pos = 0;
        self.history_index = None;
        self.saved_draft.clear();
        self.tab_state = None;
    }

    pub fn clear_line(&mut self) {
        self.text.clear();
        self.cursor_pos = 0;
    }

    pub fn move_home(&mut self) {
        self.cursor_pos = 0;
    }

    pub fn move_end(&mut self) {
        self.cursor_pos = self.text.len();
    }

    pub fn move_left(&mut self) {
        if self.cursor_pos > 0 {
            self.cursor_pos = prev_char_boundary(&self.text, self.cursor_pos);
        }
    }

    pub fn move_right(&mut self) {
        if self.cursor_pos < self.text.len() {
            self.cursor_pos = next_char_boundary(&self.text, self.cursor_pos);
        }
    }

    pub fn backspace(&mut self) {
        if self.cursor_pos > 0 {
            let prev = prev_char_boundary(&self.text, self.cursor_pos);
            self.text.replace_range(prev..self.cursor_pos, "");
            self.cursor_pos = prev;
        }
    }

    pub fn delete_forward(&mut self) {
        if self.cursor_pos < self.text.len() {
            let next = next_char_boundary(&self.text, self.cursor_pos);
            self.text.replace_range(self.cursor_pos..next, "");
        }
    }

    pub fn insert_char(&mut self, ch: char) {
        self.text.insert(self.cursor_pos, ch);
        self.cursor_pos += ch.len_utf8();
    }

    pub fn delete_word_backwards(&mut self) {
        if self.cursor_pos == 0 {
            return;
        }
        let before = &self.text[..self.cursor_pos];
        let trimmed_ws = before.trim_end_matches(char::is_whitespace);
        let word_start = trimmed_ws
            .char_indices()
            .rev()
            .find(|(_, c)| c.is_whitespace())
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(0);
        self.text.replace_range(word_start..self.cursor_pos, "");
        self.cursor_pos = word_start;
    }

    pub fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let next_idx = match self.history_index {
            None => {
                self.saved_draft = self.text.clone();
                self.history.len() - 1
            }
            Some(0) => 0,
            Some(i) => i - 1,
        };
        self.history_index = Some(next_idx);
        self.set_text(self.history[next_idx].clone());
    }

    pub fn history_next(&mut self) {
        let Some(idx) = self.history_index else {
            return;
        };
        if idx + 1 < self.history.len() {
            let next_idx = idx + 1;
            self.history_index = Some(next_idx);
            self.set_text(self.history[next_idx].clone());
        } else {
            self.history_index = None;
            let draft = std::mem::take(&mut self.saved_draft);
            self.set_text(draft);
        }
    }

    pub fn complete_tab(&mut self, users: &[String]) {
        if let Some(mut state) = self.tab_state.take() {
            if !state.candidates.is_empty() {
                state.index = (state.index + 1) % state.candidates.len();
                let replacement = &state.candidates[state.index];
                let end = state.replace_start + state.replace_len;
                self.text
                    .replace_range(state.replace_start..end, replacement);
                state.replace_len = replacement.len();
                self.cursor_pos = state.replace_start + state.replace_len;
                self.tab_state = Some(state);
            }
            return;
        }

        let before_cursor = &self.text[..self.cursor_pos];
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
            slash_command_names()
                .filter(|cmd| cmd.starts_with(&lower))
                .map(|cmd| format!("{cmd} "))
                .collect()
        } else {
            let lower = token.to_ascii_lowercase();
            let suffix = if token_start == 0 { ": " } else { " " };
            users
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
        self.text
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

    pub fn take_submitted(&mut self) -> Option<String> {
        let line = std::mem::take(&mut self.text);
        self.cursor_pos = 0;
        self.history_index = None;
        self.saved_draft.clear();

        if line.trim().is_empty() {
            return None;
        }

        if self.history.last() != Some(&line) {
            self.history.push(line.clone());
        }

        Some(line)
    }
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
