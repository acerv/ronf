// SPDX-License-Identifier: GPL-3.0-or-later

pub mod app;
pub mod config;
pub mod ui;

use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyEventKind};
use futures::StreamExt;
use irc::client::prelude::Client;
use ratatui::DefaultTerminal;
use tokio::sync::mpsc;

use crate::app::{App, ServerEvent};
use crate::config::AppConfig;

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

        tokio::spawn(async move {
            loop {
                match Client::from_config(irc_cfg.clone()).await {
                    Ok(mut client) => {
                        if let Err(err) = client.identify() {
                            let _ = tx.send(ServerEvent::Error {
                                server_id: server_id.clone(),
                                error: format!("Failed to identify: {err} (retrying in 5s)"),
                            });
                            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                            continue;
                        }

                        let sender = client.sender();
                        if tx
                            .send(ServerEvent::Connected {
                                server_id: server_id.clone(),
                                sender,
                            })
                            .is_err()
                        {
                            break;
                        }

                        let mut stream = match client.stream() {
                            Ok(s) => s,
                            Err(err) => {
                                let _ = tx.send(ServerEvent::Error {
                                    server_id: server_id.clone(),
                                    error: format!("Failed to open IRC stream: {err} (retrying in 5s)"),
                                });
                                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                                continue;
                            }
                        };

                        while let Some(item) = stream.next().await {
                            match item {
                                Ok(message) => {
                                    if tx
                                        .send(ServerEvent::Message {
                                            server_id: server_id.clone(),
                                            message: Box::new(message),
                                        })
                                        .is_err()
                                    {
                                        return;
                                    }
                                }
                                Err(err) => {
                                    let _ = tx.send(ServerEvent::Error {
                                        server_id: server_id.clone(),
                                        error: format!("Disconnected: {err} (reconnecting in 5s)"),
                                    });
                                    break;
                                }
                            }
                        }
                    }
                    Err(err) => {
                        if tx
                            .send(ServerEvent::Error {
                                server_id: server_id.clone(),
                                error: format!("Connection failed: {err} (retrying in 5s)"),
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        });
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
