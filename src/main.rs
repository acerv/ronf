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
                    Some(Ok(Event::Resize(_, _))) => {}
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

    while let Some(item) = stream.next().await {
        match item {
            Ok(message) => {
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
            Err(err) => {
                return SessionEnd::Retry(format!("Disconnected: {err} (reconnecting in 5s)"));
            }
        }
    }

    SessionEnd::Retry("Connection closed (reconnecting in 5s)".to_string())
}
