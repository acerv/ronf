// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::buffer::{is_channel_name, Buffer, BufferKind, ChatLine, LineKind};

fn sanitize_log_component(raw: &str, allow_chan_prefix: bool) -> String {
    let mut out = String::with_capacity(raw.len());
    for &b in raw.as_bytes() {
        if b.is_ascii_alphanumeric()
            || matches!(b, b'-' | b'_')
            || (allow_chan_prefix && matches!(b, b'#' | b'&'))
        {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(char::from_digit((b >> 4) as u32, 16).unwrap().to_ascii_uppercase());
            out.push(char::from_digit((b & 0x0F) as u32, 16).unwrap().to_ascii_uppercase());
        }
    }
    out
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let d1 = (bytes[i + 1] as char).to_digit(16);
            let d2 = (bytes[i + 2] as char).to_digit(16);
            if let (Some(d1), Some(d2)) = (d1, d2) {
                decoded.push(((d1 << 4) | d2) as u8);
                i += 3;
                continue;
            }
        }
        decoded.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(decoded)
        .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
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
        let r1 = set.remove(&key);
        let r2 = set.remove(&dm_name.to_ascii_lowercase());
        r1 || r2
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
            let name = percent_decode(&stem);
            if stem.is_empty()
                || name.is_empty()
                || is_channel_name(&stem)
                || is_channel_name(&name)
                || closed_dms.contains(&stem.to_ascii_lowercase())
                || closed_dms.contains(&name.to_ascii_lowercase())
            {
                return None;
            }
            let already_exists = existing.iter().any(|b| {
                b.kind != BufferKind::Server
                    && b.server_id.eq_ignore_ascii_case(server_id)
                    && b.name.eq_ignore_ascii_case(&name)
            });
            (!already_exists).then_some(name)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_and_decodes_special_nick_characters() {
        assert_eq!(sanitize_log_component("alice[m]", true), "alice%5Bm%5D");
        assert_eq!(sanitize_log_component("bob|afk", true), "bob%7Cafk");
        assert_eq!(sanitize_log_component("#rust", true), "#rust");
        assert_eq!(sanitize_log_component("#c++", true), "#c%2B%2B");
        assert_eq!(sanitize_log_component("server#1", false), "server%231");

        assert_eq!(percent_decode("alice%5Bm%5D"), "alice[m]");
        assert_eq!(percent_decode("bob%7Cafk"), "bob|afk");
        assert_eq!(percent_decode("plain_nick"), "plain_nick");
    }
}
