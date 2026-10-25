// SPDX-License-Identifier: GPL-3.0-or-later

use crate::buffer::is_channel_name;

pub struct CommandSpec {
    pub names: &'static [&'static str],
    pub usage: &'static str,
    pub description: &'static str,
}

pub const COMMAND_SPECS: &[CommandSpec] = &[
    CommandSpec {
        names: &["/help"],
        usage: "/help",
        description: "Open this help dialog",
    },
    CommandSpec {
        names: &["/join"],
        usage: "/join <#chan>",
        description: "Join channel on active server",
    },
    CommandSpec {
        names: &["/part", "/leave"],
        usage: "/part [reason]",
        description: "Leave current channel",
    },
    CommandSpec {
        names: &["/msg"],
        usage: "/msg <to> <text>",
        description: "Send private or channel message",
    },
    CommandSpec {
        names: &["/query"],
        usage: "/query <nick>",
        description: "Open direct message buffer",
    },
    CommandSpec {
        names: &["/nick"],
        usage: "/nick <newnick>",
        description: "Change nick on active server",
    },
    CommandSpec {
        names: &["/me"],
        usage: "/me <action>",
        description: "Send CTCP action message",
    },
    CommandSpec {
        names: &["/topic"],
        usage: "/topic [text]",
        description: "View or set channel topic",
    },
    CommandSpec {
        names: &["/whois"],
        usage: "/whois <nick>",
        description: "Query user info via WHOIS",
    },
    CommandSpec {
        names: &["/kick"],
        usage: "/kick <nick> [msg]",
        description: "Kick user from active channel",
    },
    CommandSpec {
        names: &["/invite"],
        usage: "/invite <nick>",
        description: "Invite user to channel",
    },
    CommandSpec {
        names: &["/mode"],
        usage: "/mode <modes>",
        description: "Set channel or user modes",
    },
    CommandSpec {
        names: &["/op", "/deop"],
        usage: "/op / /deop <nick>",
        description: "Grant or revoke operator (@)",
    },
    CommandSpec {
        names: &["/voice", "/devoice"],
        usage: "/voice / /devoice",
        description: "Grant or revoke voice (+)",
    },
    CommandSpec {
        names: &["/ban"],
        usage: "/ban <mask>",
        description: "Ban hostmask on channel (+b)",
    },
    CommandSpec {
        names: &["/away"],
        usage: "/away [reason]",
        description: "Set or clear away status",
    },
    CommandSpec {
        names: &["/list"],
        usage: "/list [filter]",
        description: "List channels on server",
    },
    CommandSpec {
        names: &["/motd"],
        usage: "/motd",
        description: "Request server Message of the Day",
    },
    CommandSpec {
        names: &["/buffer"],
        usage: "/buffer <target>",
        description: "Switch buffer by name or number",
    },
    CommandSpec {
        names: &["/reconnect"],
        usage: "/reconnect",
        description: "Reconnect active server",
    },
    CommandSpec {
        names: &["/ignore"],
        usage: "/ignore [nick]",
        description: "Ignore user or list ignored",
    },
    CommandSpec {
        names: &["/unignore"],
        usage: "/unignore <nick>",
        description: "Stop ignoring user",
    },
    CommandSpec {
        names: &["/search"],
        usage: "/search <query>",
        description: "Search active buffer messages",
    },
    CommandSpec {
        names: &["/raw", "/quote"],
        usage: "/raw <irc_line>",
        description: "Send raw IRC protocol line",
    },
    CommandSpec {
        names: &["/clear"],
        usage: "/clear",
        description: "Clear active buffer history",
    },
    CommandSpec {
        names: &["/close"],
        usage: "/close",
        description: "Close active channel or DM",
    },
    CommandSpec {
        names: &["/quit", "/exit"],
        usage: "/quit [reason]",
        description: "Disconnect all and exit",
    },
];

pub fn slash_command_names() -> impl Iterator<Item = &'static str> {
    COMMAND_SPECS.iter().flat_map(|spec| spec.names.iter().copied())
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
    Mode {
        target: Option<String>,
        args: String,
        channel_only: bool,
    },
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

fn non_empty(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_string())
}

fn first_word(s: &str) -> Option<&str> {
    s.split_whitespace().next()
}

fn split_first_arg(s: &str) -> Option<(String, Option<String>)> {
    if s.is_empty() {
        return None;
    }
    match s.split_once(char::is_whitespace) {
        Some((first, tail)) => Some((first.to_string(), non_empty(tail.trim()))),
        None => Some((s.to_string(), None)),
    }
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
            let chan = first_word(rest)?;
            let normalized = if is_channel_name(chan) {
                chan.to_string()
            } else {
                format!("#{chan}")
            };
            Some(UserCommand::Join(normalized))
        }
        "/part" | "/leave" => Some(UserCommand::Part(non_empty(rest))),
        "/msg" => {
            let (target, Some(text)) = split_first_arg(rest)? else {
                return None;
            };
            Some(UserCommand::Msg { target, text })
        }
        "/query" => {
            let (target, text) = split_first_arg(rest)?;
            Some(UserCommand::Query { target, text })
        }
        "/nick" => Some(UserCommand::Nick(first_word(rest)?.to_string())),
        "/me" => non_empty(rest).map(UserCommand::Me),
        "/topic" => Some(UserCommand::Topic(non_empty(rest))),
        "/whois" => Some(UserCommand::Whois(first_word(rest)?.to_string())),
        "/kick" => {
            let (nick, reason) = split_first_arg(rest)?;
            Some(UserCommand::Kick { nick, reason })
        }
        "/invite" => {
            let (nick, channel) = split_first_arg(rest)?;
            Some(UserCommand::Invite { nick, channel })
        }
        "/mode" => {
            if rest.is_empty() {
                Some(UserCommand::Mode {
                    target: None,
                    args: String::new(),
                    channel_only: false,
                })
            } else {
                let (first, rem) = split_first_arg(rest)?;
                if is_channel_name(&first) {
                    Some(UserCommand::Mode {
                        target: Some(first),
                        args: rem.unwrap_or_default(),
                        channel_only: false,
                    })
                } else if !first.starts_with(['+', '-'])
                    && rem.as_deref().map_or(false, |r| r.starts_with(['+', '-']))
                {
                    Some(UserCommand::Mode {
                        target: Some(first),
                        args: rem.unwrap_or_default(),
                        channel_only: false,
                    })
                } else {
                    Some(UserCommand::Mode {
                        target: None,
                        args: rest.to_string(),
                        channel_only: false,
                    })
                }
            }
        }
        "/ban" | "/op" | "/deop" | "/voice" | "/devoice" => {
            let arg = first_word(rest)?;
            let flag = match cmd.as_str() {
                "/ban" => "+b",
                "/op" => "+o",
                "/deop" => "-o",
                "/voice" => "+v",
                _ => "-v",
            };
            Some(UserCommand::Mode {
                target: None,
                args: format!("{flag} {arg}"),
                channel_only: true,
            })
        }
        "/away" => Some(UserCommand::Away(non_empty(rest))),
        "/motd" => Some(UserCommand::Motd),
        "/list" => Some(UserCommand::List(non_empty(rest))),
        "/buffer" | "/b" => non_empty(rest).map(UserCommand::Buffer),
        "/reconnect" => Some(UserCommand::Reconnect),
        "/ignore" => Some(UserCommand::Ignore(first_word(rest).map(str::to_string))),
        "/unignore" => Some(UserCommand::Unignore(first_word(rest)?.to_string())),
        "/search" => non_empty(rest).map(UserCommand::Search),
        "/raw" | "/quote" => non_empty(rest).map(UserCommand::Raw),
        "/clear" => Some(UserCommand::Clear),
        "/close" => Some(UserCommand::Close),
        "/quit" | "/exit" => Some(UserCommand::Quit(non_empty(rest))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            parse_user_command("/mode"),
            Some(UserCommand::Mode {
                target: None,
                args: String::new(),
                channel_only: false,
            })
        );
        assert_eq!(
            parse_user_command("/mode mynick +i"),
            Some(UserCommand::Mode {
                target: Some("mynick".to_string()),
                args: "+i".to_string(),
                channel_only: false,
            })
        );
        assert_eq!(
            parse_user_command("/mode #rust +m"),
            Some(UserCommand::Mode {
                target: Some("#rust".to_string()),
                args: "+m".to_string(),
                channel_only: false,
            })
        );
        assert_eq!(
            parse_user_command("/mode +i"),
            Some(UserCommand::Mode {
                target: None,
                args: "+i".to_string(),
                channel_only: false,
            })
        );
        assert_eq!(
            parse_user_command("/op alice"),
            Some(UserCommand::Mode {
                target: None,
                args: "+o alice".to_string(),
                channel_only: true,
            })
        );
        assert_eq!(
            parse_user_command("/ban *!*@bad.host"),
            Some(UserCommand::Mode {
                target: None,
                args: "+b *!*@bad.host".to_string(),
                channel_only: true,
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
}
