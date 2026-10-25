// SPDX-License-Identifier: GPL-3.0-or-later

pub mod app;
pub mod buffer;
pub mod command;
pub mod config;
pub mod input;
pub mod irc_handler;
pub mod log_store;
pub mod ui;

use std::time::Duration;

use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyEventKind};
use futures::StreamExt;
use irc::client::prelude::{Client, Config as IrcConfig};
use irc::error::Error as IrcError;
use irc::proto::{Command, Response};
use ratatui::DefaultTerminal;
use tokio::sync::mpsc;

use crate::app::App;
use crate::config::AppConfig;
use crate::irc_handler::ServerEvent;

#[tokio::main]
async fn main() -> Result<()> {
    let cli_arg = std::env::args().nth(1);
    let config_path = AppConfig::resolve_path(cli_arg.as_deref());
    let app_config = AppConfig::from_file(&config_path)?;

    let mut terminal = ratatui::try_init()?;
    let res = run_app(&mut terminal, app_config).await;
    ratatui::restore();
    res
}

async fn run_app(terminal: &mut DefaultTerminal, app_config: AppConfig) -> Result<()> {
    let mut app = App::from_config(&app_config);
    app.set_terminal_height(terminal.size()?.height);
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerEvent>();

    for (server_id, srv_cfg) in app_config.enabled_servers() {
        let server_id = server_id.clone();
        let irc_cfg = srv_cfg.to_irc_config(app_config.default_nickname.as_deref())?;
        let tx = tx.clone();
        tokio::spawn(run_server_loop(server_id, irc_cfg, tx));
    }
    drop(tx);

    let mut term_events = EventStream::new();

    terminal.draw(|f| ui::draw(f, &app))?;

    loop {
        tokio::select! {
            maybe_term = term_events.next() => {
                match maybe_term {
                    Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => {
                        app.handle_key_event(key);
                    }
                    Some(Ok(Event::Resize(_, h))) => {
                        app.set_terminal_height(h);
                    }
                    Some(Err(_)) | None => {
                        app.should_quit = true;
                    }
                    _ => {}
                }
            }
            Some(srv_event) = rx.recv() => {
                app.handle_server_event(srv_event);
            }
        }

        terminal.draw(|f| ui::draw(f, &app))?;

        if app.should_quit {
            break;
        }
    }

    Ok(())
}

enum SessionEnd {
    ReceiverClosed,
    Retry(String),
}

async fn run_server_loop(
    server_id: String,
    irc_cfg: IrcConfig,
    tx: mpsc::UnboundedSender<ServerEvent>,
) {
    loop {
        match connect_and_stream(&server_id, irc_cfg.clone(), &tx).await {
            SessionEnd::ReceiverClosed => break,
            SessionEnd::Retry(error) => {
                if tx
                    .send(ServerEvent::Error {
                        server_id: server_id.clone(),
                        error,
                    })
                    .is_err()
                {
                    break;
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

async fn connect_and_stream(
    server_id: &str,
    irc_cfg: IrcConfig,
    tx: &mpsc::UnboundedSender<ServerEvent>,
) -> SessionEnd {
    let mut client = match Client::from_config(irc_cfg).await {
        Ok(c) => c,
        Err(err) => return SessionEnd::Retry(format!("Connection failed: {err} (retrying in 5s)")),
    };

    if let Err(err) = client.identify() {
        return SessionEnd::Retry(format!("Failed to identify: {err} (retrying in 5s)"));
    }

    if tx
        .send(ServerEvent::Connected {
            server_id: server_id.to_string(),
            sender: client.sender(),
        })
        .is_err()
    {
        return SessionEnd::ReceiverClosed;
    }

    let mut stream = match client.stream() {
        Ok(s) => s,
        Err(err) => {
            return SessionEnd::Retry(format!(
                "Failed to open IRC stream: {err} (retrying in 5s)"
            ));
        }
    };

    let mut registered = false;

    while let Some(item) = stream.next().await {
        match item {
            Ok(message) => {
                if matches!(
                    message.command,
                    Command::Response(Response::RPL_WELCOME, _)
                ) {
                    registered = true;
                }
                if tx
                    .send(ServerEvent::Message {
                        server_id: server_id.to_string(),
                        message: Box::new(message),
                    })
                    .is_err()
                {
                    return SessionEnd::ReceiverClosed;
                }
            }
            Err(IrcError::NoUsableNick) => {
                if tx
                    .send(ServerEvent::NickRejected {
                        server_id: server_id.to_string(),
                        registered,
                    })
                    .is_err()
                {
                    return SessionEnd::ReceiverClosed;
                }
            }
            Err(err) => {
                return SessionEnd::Retry(format!("Disconnected: {err} (reconnecting in 5s)"));
            }
        }
    }

    SessionEnd::Retry("Connection closed (reconnecting in 5s)".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn survives_nick_collisions_before_and_after_welcome() {
        let irc_cfg = IrcConfig {
            nickname: Some("irc_user".into()),
            server: Some("irc.example.org".into()),
            use_tls: Some(false),
            use_mock_connection: true,
            mock_initial_value: Some(
                ":irc.test 433 * irc_user :Nickname is already in use.\r\n\
                 :irc.test 001 irc_user_ :Welcome\r\n\
                 :irc.test 433 irc_user_ alice :Nickname is already in use.\r\n\
                 :alice!a@h PRIVMSG #rust :still connected\r\n"
                    .into(),
            ),
            ..IrcConfig::default()
        };

        let toml = r##"
            default_nickname = "irc_user"
            [servers.libera]
            server = "irc.example.org"
            channels = ["#rust"]
        "##;
        let mut app = App::from_config(&AppConfig::from_toml_str(toml).unwrap());
        let (tx, mut rx) = mpsc::unbounded_channel();

        let end = connect_and_stream("libera", irc_cfg, &tx).await;
        assert!(matches!(end, SessionEnd::Retry(_)));
        drop(tx);

        while let Some(ev) = rx.recv().await {
            app.handle_server_event(ev);
        }

        assert_eq!(app.nick_for("libera"), "irc_user_");
        let rust_idx = app.find_buffer("libera", "#rust").unwrap();
        assert_eq!(app.buffers[rust_idx].messages.len(), 1);
        assert_eq!(app.buffers[rust_idx].messages[0].content, "still connected");
    }
}
