// SPDX-License-Identifier: GPL-3.0-or-later

pub mod app;
pub mod config;
pub mod ui;

use std::io;
use std::panic;

use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use futures::StreamExt;
use irc::client::prelude::Client;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

use crate::app::{App, ServerEvent};
use crate::config::AppConfig;

#[tokio::main]
async fn main() -> Result<()> {
    let cli_arg = std::env::args().nth(1);
    let config_path = AppConfig::resolve_path(cli_arg.as_deref());
    let app_config = AppConfig::from_file(&config_path)?;

    install_panic_hook();

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    if let Err(err) = execute!(stdout, EnterAlternateScreen) {
        let _ = disable_raw_mode();
        return Err(err.into());
    }
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = match Terminal::new(backend) {
        Ok(t) => t,
        Err(err) => {
            let _ = disable_raw_mode();
            let _ = execute!(io::stdout(), LeaveAlternateScreen);
            return Err(err.into());
        }
    };

    let res = run_app(&mut terminal, app_config).await;

    let _ = disable_raw_mode();
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
    let _ = terminal.show_cursor();

    res
}

fn install_panic_hook() {
    let default_hook = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        default_hook(info);
    }));
}

async fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app_config: AppConfig,
) -> Result<()> {
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
