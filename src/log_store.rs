// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::buffer::{is_channel_name, Buffer, BufferKind, ChatLine, LineKind};

fn sanitize_log_component(raw: &str, allow_chan_prefix: bool) -> String {
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric()
                || matches!(c, '-' | '_')
                || (allow_chan_prefix && matches!(c, '#' | '&'))
            {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn server_log_dir(base_dir: &Path, server_id: &str) -> PathBuf {
    base_dir.join(sanitize_log_component(server_id, false))
}

fn buffer_log_path(base_dir: &Path, server_id: &str, buf_name: &str) -> PathBuf {
    let safe_buf = sanitize_log_component(buf_name, true);
    server_log_dir(base_dir, server_id).join(format!("{safe_buf}.log"))
}

fn closed_dms_path(base_dir: &Path, server_id: &str) -> PathBuf {
    server_log_dir(base_dir, server_id).join(".closed_dms")
}

fn read_closed_dms(base_dir: &Path, server_id: &str) -> BTreeSet<String> {
    fs::read_to_string(closed_dms_path(base_dir, server_id))
        .map(|s| {
            s.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(|l| l.to_ascii_lowercase())
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) fn set_dm_closed(base_dir: &Path, server_id: &str, dm_name: &str, closed: bool) {
    let key = sanitize_log_component(dm_name, true).to_ascii_lowercase();
    let mut set = read_closed_dms(base_dir, server_id);
    let changed = if closed {
        set.insert(key)
    } else {
        set.remove(&key)
    };
    if !changed {
        return;
    }
    let path = closed_dms_path(base_dir, server_id);
    if set.is_empty() {
        let _ = fs::remove_file(path);
        return;
    }
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let body: String = set.into_iter().map(|s| format!("{s}\n")).collect();
    let _ = fs::write(path, body);
}

pub(crate) fn append_buffer_log(base_dir: &Path, server_id: &str, buf_name: &str, line: &ChatLine) {
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

fn parse_log_line(raw: &str) -> Option<ChatLine> {
    let rest = raw.strip_prefix('[')?;
    let (ts, after_ts) = rest.split_once("] ")?;
    if let Some(action_rest) = after_ts.strip_prefix("* ") {
        let (actor, act_text) = action_rest.split_once(' ')?;
        Some(ChatLine {
            timestamp: ts.to_string(),
            sender: Some(actor.to_string()),
            content: act_text.to_string(),
            kind: LineKind::Action,
        })
    } else if let Some(nick_rest) = after_ts.strip_prefix('<') {
        let (sender, msg_text) = nick_rest.split_once("> ")?;
        Some(ChatLine {
            timestamp: ts.to_string(),
            sender: Some(sender.to_string()),
            content: msg_text.to_string(),
            kind: LineKind::Normal,
        })
    } else {
        None
    }
}

pub(crate) fn load_more_buffer_history(base_dir: &Path, buf: &mut Buffer, limit: usize) -> usize {
    if buf.kind == BufferKind::Server || limit == 0 || buf.history_cursor == Some(0) {
        return 0;
    }
    let path = buffer_log_path(base_dir, &buf.server_id, &buf.name);
    let Ok(content) = fs::read_to_string(path) else {
        buf.history_cursor = Some(0);
        return 0;
    };
    let all_lines: Vec<&str> = content.lines().collect();
    let end = buf
        .history_cursor
        .unwrap_or(all_lines.len())
        .min(all_lines.len());
    if end == 0 {
        buf.history_cursor = Some(0);
        return 0;
    }

    let mut batch = Vec::with_capacity(limit.min(end));
    let mut new_cursor = 0usize;
    for idx in (0..end).rev() {
        if let Some(line) = parse_log_line(all_lines[idx]) {
            batch.push(line);
            if batch.len() == limit {
                new_cursor = idx;
                break;
            }
        }
    }

    buf.history_cursor = Some(new_cursor);
    batch.reverse();
    let loaded = batch.len();
    if loaded > 0 {
        buf.messages.splice(0..0, batch);
    }
    loaded
}

pub(crate) fn discover_dm_buffers(
    base_dir: &Path,
    server_id: &str,
    existing: &[Buffer],
    limit: usize,
) -> Vec<Buffer> {
    if limit == 0 {
        return Vec::new();
    }
    let srv_dir = server_log_dir(base_dir, server_id);
    let Ok(entries) = fs::read_dir(srv_dir) else {
        return Vec::new();
    };
    let closed_dms = read_closed_dms(base_dir, server_id);

    let mut dm_names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let path = e.path();
            if path.extension().and_then(|s| s.to_str()) != Some("log") {
                return None;
            }
            let stem = path.file_stem()?.to_str()?.to_string();
            if stem.is_empty()
                || is_channel_name(&stem)
                || closed_dms.contains(&stem.to_ascii_lowercase())
            {
                return None;
            }
            let already_exists = existing.iter().any(|b| {
                b.kind != BufferKind::Server
                    && b.server_id.eq_ignore_ascii_case(server_id)
                    && b.name.eq_ignore_ascii_case(&stem)
            });
            (!already_exists).then_some(stem)
        })
        .collect();

    dm_names.sort_by_key(|a| a.to_ascii_lowercase());

    let mut dm_buffers = Vec::new();
    for name in dm_names {
        let mut dm_buf = Buffer::new(server_id.to_string(), name, BufferKind::DirectMessage);
        if load_more_buffer_history(base_dir, &mut dm_buf, limit) > 0 {
            dm_buffers.push(dm_buf);
        }
    }
    dm_buffers
}
