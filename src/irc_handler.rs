// SPDX-License-Identifier: GPL-3.0-or-later

use irc::client::Sender;
use irc::proto::{Command, Message, Prefix, Response};

use crate::app::App;
use crate::buffer::{contains_nick_mention, is_channel_name, Buffer, BufferKind, ChatLine, LineKind};

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
    NickRejected {
        server_id: String,
        registered: bool,
    },
    Error {
        server_id: String,
        error: String,
    },
}

impl App {
    pub(crate) fn affected_server_buffers(
        &mut self,
        server_id: &str,
        mut f: impl FnMut(&mut Buffer) -> bool,
    ) -> Vec<usize> {
        self.buffers
            .iter_mut()
            .enumerate()
            .filter_map(|(i, buf)| {
                (buf.server_id.eq_ignore_ascii_case(server_id) && f(buf)).then_some(i)
            })
            .collect()
    }

    fn set_channel_topic(
        &mut self,
        server_id: &str,
        chan: &str,
        topic: Option<String>,
        announce_empty: bool,
    ) {
        let idx = if let Some(chan_idx) = self.find_buffer(server_id, chan) {
            self.buffers[chan_idx].topic = topic.clone();
            chan_idx
        } else {
            self.target_buffer_for_server(server_id)
        };
        match topic {
            Some(t) => self.push_system(idx, format!("* Topic for {chan}: {t}")),
            None if announce_empty => {
                self.push_system(idx, format!("* No topic is set for {chan}"))
            }
            None => {}
        }
    }

    fn push_whois(&mut self, idx: usize, nick: &str, detail: impl std::fmt::Display) {
        self.push_system(idx, format!("* [whois] {nick} {detail}"));
    }

    fn push_whois_tail(&mut self, idx: usize, args: &[String]) {
        if args.len() >= 3 {
            self.push_whois(idx, &args[1], args[2..].join(" "));
        }
    }

    pub(crate) fn reset_server_channels(&mut self, server_id: &str) {
        self.affected_server_buffers(server_id, |buf| {
            buf.users.clear();
            buf.pending_names.clear();
            buf.user_scroll_offset = 0;
            buf.topic = None;
            false
        });
    }

    pub fn handle_server_event(&mut self, event: ServerEvent) {
        match event {
            ServerEvent::Connected { server_id, sender } => {
                self.reset_server_channels(&server_id);
                self.set_sender(&server_id, Some(sender));
                if let Some(idx) = self.find_server_buffer(&server_id) {
                    self.push_system(idx, format!("Connected to [{server_id}]"));
                }
            }
            ServerEvent::Error { server_id, error } => {
                self.set_sender(&server_id, None);
                self.reset_server_channels(&server_id);
                if let Some(idx) = self.find_server_buffer(&server_id) {
                    self.push_error(idx, error);
                }
            }
            ServerEvent::Message { server_id, message } => {
                self.handle_irc_message(&server_id, *message);
            }
            ServerEvent::NickRejected {
                server_id,
                registered,
            } => {
                self.handle_nick_rejected(&server_id, registered);
            }
        }
    }

    pub fn handle_nick_rejected(&mut self, server_id: &str, registered: bool) {
        if registered {
            let idx = self.target_buffer_for_server(server_id);
            self.push_error(idx, "! Nickname is already in use or invalid");
            return;
        }

        let current = self.nick_for(server_id).to_string();
        let trailing_underscores = current.bytes().rev().take_while(|&b| b == b'_').count();
        let idx = self.find_server_buffer(server_id).unwrap_or(self.active_buffer);
        if trailing_underscores >= 5 || current.len() >= 30 {
            self.push_error(
                idx,
                format!("! Nickname '{current}' rejected; use /nick to choose another"),
            );
            return;
        }

        let fallback = format!("{current}_");
        self.set_nick(server_id, fallback.clone());
        if let Some(sender) = self.sender_for(server_id) {
            let _ = sender.send(Command::NICK(fallback.clone()));
        }
        self.push_error(
            idx,
            format!("! Nickname '{current}' in use; switching to '{fallback}'"),
        );
    }

    pub fn handle_irc_message(&mut self, server_id: &str, msg: Message) {
        let sender_nick = extract_nick(&msg.prefix);
        let is_ignored = sender_nick
            .as_deref()
            .is_some_and(|n| self.ignored_nicks.contains(&n.to_ascii_lowercase()));
        let my_nick = self.nick_for(server_id).to_string();

        match msg.command {
            Command::PRIVMSG(_, _) | Command::NOTICE(_, _) if is_ignored => {}
            Command::PRIVMSG(target, text) => {
                let is_channel = is_channel_name(&target);
                let ctcp_action = match parse_ctcp(&text) {
                    Some(CtcpPayload::Action(act)) => Some(act.to_string()),
                    Some(CtcpPayload::Other(ctcp)) => {
                        let idx = if is_channel {
                            self.find_buffer(server_id, &target)
                                .unwrap_or_else(|| self.target_buffer_for_server(server_id))
                        } else {
                            self.find_server_buffer(server_id)
                                .unwrap_or(self.active_buffer)
                        };
                        let from = sender_nick.as_deref().unwrap_or("someone");
                        self.push_system(idx, format!("* [ctcp] {from}: {ctcp}"));
                        return;
                    }
                    None => None,
                };

                let is_from_self = sender_nick
                    .as_deref()
                    .is_some_and(|s| s.eq_ignore_ascii_case(&my_nick));
                let (buf_name, buf_kind) = if is_channel {
                    (target, BufferKind::Channel)
                } else if is_from_self {
                    (target, BufferKind::DirectMessage)
                } else {
                    (
                        sender_nick.clone().unwrap_or(target),
                        BufferKind::DirectMessage,
                    )
                };

                let is_new_dm = !is_channel && self.find_buffer(server_id, &buf_name).is_none();
                let idx = self.ensure_buffer(server_id, &buf_name, buf_kind);

                let display_text = ctcp_action.as_deref().unwrap_or(&text);
                let is_mentioned =
                    is_channel && !is_from_self && contains_nick_mention(display_text, &my_nick);

                if !is_from_self && (is_new_dm || !is_channel || is_mentioned) {
                    if idx != self.active_buffer || self.buffers[idx].scroll_offset > 0 {
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

                let (content, kind) = match ctcp_action {
                    Some(action) => (action, LineKind::Action),
                    None => (text, LineKind::Normal),
                };
                self.push_to_buffer(idx, ChatLine::new(sender_nick, content, kind));
            }
            Command::NOTICE(target, text) => {
                if let Some(CtcpPayload::Other(ctcp) | CtcpPayload::Action(ctcp)) =
                    parse_ctcp(&text)
                {
                    let idx = self.target_buffer_for_server(server_id);
                    let from = sender_nick.as_deref().unwrap_or("someone");
                    self.push_system(idx, format!("* [ctcp] {from} reply: {ctcp}"));
                    return;
                }
                let idx = if is_channel_name(&target) {
                    self.ensure_buffer(server_id, &target, BufferKind::Channel)
                } else {
                    self.find_server_buffer(server_id).unwrap_or(0)
                };
                let prefix = sender_nick.map(|n| format!("-{n}- {text}")).unwrap_or(text);
                self.push_system(idx, prefix);
            }
            Command::JOIN(chan, _, _) => {
                let idx = self.ensure_buffer(server_id, &chan, BufferKind::Channel);
                if let Some(nick) = sender_nick {
                    if nick.eq_ignore_ascii_case(&my_nick) {
                        self.buffers[idx].parted = false;
                    }
                    self.buffers[idx].add_user(&nick);
                    if self.should_show_join_part(server_id) {
                        self.push_system(idx, format!("--> {nick} has joined {chan}"));
                    }
                }
            }
            Command::PART(chan, reason) => {
                if let Some(idx) = self.find_buffer(server_id, &chan) {
                    if let Some(nick) = sender_nick {
                        if nick.eq_ignore_ascii_case(&my_nick) {
                            self.buffers[idx].mark_parted();
                        } else {
                            self.buffers[idx].remove_user(&nick);
                        }
                        if self.should_show_join_part(server_id) {
                            let suffix = format_reason(reason);
                            self.push_system(idx, format!("<-- {nick} has left {chan}{suffix}"));
                        }
                    }
                }
            }
            Command::KICK(chan, kicked, reason) => {
                if let Some(idx) = self.find_buffer(server_id, &chan) {
                    if kicked.eq_ignore_ascii_case(&my_nick) {
                        self.buffers[idx].mark_parted();
                    } else {
                        self.buffers[idx].remove_user(&kicked);
                    }
                    let by = sender_nick.unwrap_or_else(|| "server".to_string());
                    let suffix = format_reason(reason);
                    self.push_system(
                        idx,
                        format!("<-- {kicked} was kicked from {chan} by {by}{suffix}"),
                    );
                }
            }
            Command::QUIT(reason) => {
                if let Some(nick) = sender_nick {
                    let should_show = self.should_show_join_part(server_id);
                    let suffix = format_reason(reason);
                    let affected =
                        self.affected_server_buffers(server_id, |buf| buf.remove_user(&nick));
                    if should_show {
                        for idx in affected {
                            self.push_system(idx, format!("<-- {nick} has quit{suffix}"));
                        }
                    }
                }
            }
            Command::NICK(new_nick) => {
                if let Some(old_nick) = sender_nick {
                    let is_self = old_nick.eq_ignore_ascii_case(&my_nick);
                    if is_self {
                        self.set_nick(server_id, new_nick.clone());
                    }
                    let affected = self.affected_server_buffers(server_id, |buf| {
                        buf.rename_user(&old_nick, &new_nick)
                    });
                    if is_self && affected.is_empty() {
                        let idx = self.target_buffer_for_server(server_id);
                        self.push_system(idx, format!("* {old_nick} is now known as {new_nick}"));
                    } else {
                        for idx in affected {
                            self.push_system(
                                idx,
                                format!("* {old_nick} is now known as {new_nick}"),
                            );
                        }
                    }
                }
            }
            Command::TOPIC(chan, topic) => {
                self.set_channel_topic(server_id, &chan, topic, false);
            }
            Command::INVITE(invited, chan) => {
                let by = sender_nick.unwrap_or_else(|| "someone".to_string());
                let idx = self.target_buffer_for_server(server_id);
                self.push_system(idx, format!("* {by} invited {invited} to {chan}"));
            }
            Command::ChannelMODE(chan, modes) => {
                if let Some(idx) = self.find_buffer(server_id, &chan) {
                    self.buffers[idx].apply_channel_modes(&modes);
                    let by = sender_nick.unwrap_or_else(|| "server".to_string());
                    let mode_strs: Vec<String> = modes.iter().map(|m| m.to_string()).collect();
                    self.push_system(
                        idx,
                        format!("* {by} sets mode {} on {chan}", mode_strs.join(" ")),
                    );
                }
            }
            Command::UserMODE(target, modes) => {
                let by = sender_nick.unwrap_or_else(|| "server".to_string());
                let mode_strs: Vec<String> = modes.iter().map(|m| m.to_string()).collect();
                let idx = self
                    .find_buffer(server_id, &target)
                    .unwrap_or_else(|| self.target_buffer_for_server(server_id));
                let mode_str = mode_strs.join(" ");
                let text = if mode_str.is_empty() {
                    format!("* {by} sets mode on {target}")
                } else {
                    format!("* {by} sets mode {mode_str} on {target}")
                };
                self.push_system(idx, text);
            }
            Command::Response(resp, args) => {
                self.handle_numeric_response(server_id, resp, args);
            }
            Command::Raw(code, args) => {
                self.handle_raw_numeric(server_id, code.as_str(), args);
            }
            _ => {}
        }
    }

    fn handle_raw_numeric(&mut self, server_id: &str, code: &str, args: Vec<String>) {
        let idx = self.target_buffer_for_server(server_id);
        match code {
            "330" => {
                if args.len() >= 3 {
                    self.push_whois(idx, &args[1], format!("is logged in as {}", args[2]));
                }
            }
            "378" | "671" => self.push_whois_tail(idx, &args),
            _ => {}
        }
    }

    fn handle_numeric_response(&mut self, server_id: &str, resp: Response, args: Vec<String>) {
        let srv_target = self.target_buffer_for_server(server_id);
        match resp {
            Response::RPL_CHANNELMODEIS => {
                if args.len() >= 2 {
                    let (chan, modes) = if is_channel_name(&args[0]) {
                        (&args[0], args[1..].join(" "))
                    } else if args.len() >= 3 {
                        (&args[1], args[2..].join(" "))
                    } else {
                        (&args[1], String::new())
                    };
                    let idx = self.find_buffer(server_id, chan).unwrap_or(srv_target);
                    let text = if modes.is_empty() {
                        format!("* Mode for {chan}")
                    } else {
                        format!("* Mode for {chan} is {modes}")
                    };
                    self.push_system(idx, text);
                }
            }
            Response::RPL_UMODEIS => {
                let modes = if args.len() >= 2 {
                    args[1..].join(" ")
                } else {
                    args.join(" ")
                };
                self.push_system(srv_target, format!("* User mode is {modes}"));
            }
            Response::RPL_NOTOPIC => {
                if args.len() >= 2 {
                    self.set_channel_topic(server_id, &args[1], None, true);
                }
            }
            Response::RPL_TOPIC => {
                if args.len() >= 3 {
                    self.set_channel_topic(server_id, &args[1], Some(args[2..].join(" ")), false);
                }
            }
            Response::RPL_NAMREPLY => {
                if args.len() >= 4 {
                    if let Some(idx) = self.find_buffer(server_id, &args[2]) {
                        self.buffers[idx]
                            .pending_names
                            .extend(args[3].split_whitespace().map(str::to_string));
                    } else {
                        self.push_system(
                            srv_target,
                            format!("* [names] {}: {}", args[2], args[3].trim()),
                        );
                    }
                }
            }
            Response::RPL_ENDOFNAMES => {
                if args.len() >= 2 {
                    if let Some(idx) = self.find_buffer(server_id, &args[1]) {
                        let pending = std::mem::take(&mut self.buffers[idx].pending_names);
                        self.buffers[idx].users = pending;
                        self.buffers[idx].sort_users();
                    }
                }
            }
            Response::RPL_WHOISUSER => {
                if args.len() >= 6 {
                    let real = args[5..].join(" ");
                    self.push_whois(
                        srv_target,
                        &args[1],
                        format!("({}@{}): {real}", args[2], args[3]),
                    );
                }
            }
            Response::RPL_WHOISSERVER => {
                if args.len() >= 4 {
                    let info = args[3..].join(" ");
                    self.push_whois(
                        srv_target,
                        &args[1],
                        format!("server: {} ({info})", args[2]),
                    );
                }
            }
            Response::RPL_WHOISOPERATOR => self.push_whois_tail(srv_target, &args),
            Response::RPL_WHOISIDLE => {
                if args.len() >= 3 {
                    self.push_whois(srv_target, &args[1], format!("idle: {}s", args[2]));
                }
            }
            Response::RPL_WHOISCHANNELS => {
                if args.len() >= 3 {
                    self.push_whois(
                        srv_target,
                        &args[1],
                        format!("channels: {}", args[2..].join(" ")),
                    );
                }
            }
            Response::RPL_ENDOFWHOIS => {
                if args.len() >= 2 {
                    self.push_whois(srv_target, &args[1], "End of WHOIS");
                }
            }
            Response::RPL_UNAWAY | Response::RPL_NOWAWAY => {
                self.push_system(srv_target, format!("* {}", trailing_text(&args)));
            }
            Response::RPL_LIST => {
                if args.len() >= 3 {
                    let topic = if args.len() >= 4 {
                        args[3..].join(" ")
                    } else {
                        String::new()
                    };
                    self.push_system(
                        srv_target,
                        format!("* [list] {} ({}): {topic}", args[1], args[2]),
                    );
                }
            }
            Response::RPL_LISTEND => {
                self.push_system(srv_target, "* [list] End of /LIST");
            }
            Response::ERR_NICKNAMEINUSE => {
                let registered = args.first().is_some_and(|a| a != "*");
                self.handle_nick_rejected(server_id, registered);
            }
            Response::RPL_WELCOME
            | Response::RPL_YOURHOST
            | Response::RPL_CREATED
            | Response::RPL_MOTDSTART
            | Response::RPL_MOTD
            | Response::RPL_ENDOFMOTD
            | Response::ERR_NOMOTD => {
                if resp == Response::RPL_WELCOME {
                    if let Some(welcome_nick) =
                        args.first().filter(|n| !n.is_empty() && *n != "*")
                    {
                        self.set_nick(server_id, welcome_nick.clone());
                    }
                }
                if let Some(srv_idx) = self.find_server_buffer(server_id) {
                    self.push_system(srv_idx, trailing_text(&args));
                }
                if matches!(resp, Response::RPL_ENDOFMOTD | Response::ERR_NOMOTD) {
                    self.join_server_channels(server_id);
                }
            }
            _ if resp.is_error() => {
                self.push_error(srv_target, format!("! {}", trailing_text(&args)));
            }
            _ => {}
        }
    }
}

fn trailing_text(args: &[String]) -> String {
    if args.len() > 1 {
        args[1..].join(" ")
    } else {
        args.join(" ")
    }
}

fn format_reason(reason: Option<String>) -> String {
    reason.map(|r| format!(" ({r})")).unwrap_or_default()
}

fn extract_nick(prefix: &Option<Prefix>) -> Option<String> {
    match prefix {
        Some(Prefix::Nickname(nick, _, _)) => Some(nick.clone()),
        Some(Prefix::ServerName(srv)) => Some(srv.clone()),
        None => None,
    }
}

enum CtcpPayload<'a> {
    Action(&'a str),
    Other(&'a str),
}

fn parse_ctcp(text: &str) -> Option<CtcpPayload<'_>> {
    let inner = text.strip_prefix('\x01')?;
    let inner = inner.strip_suffix('\x01').unwrap_or(inner);
    if inner.eq_ignore_ascii_case("ACTION") {
        Some(CtcpPayload::Action(""))
    } else if inner
        .get(..7)
        .is_some_and(|p| p.eq_ignore_ascii_case("ACTION "))
    {
        Some(CtcpPayload::Action(&inner[7..]))
    } else {
        Some(CtcpPayload::Other(inner))
    }
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
