// SPDX-License-Identifier: GPL-3.0-or-later

pub mod app;
pub mod buffer;
pub mod command;
pub mod config;
pub mod input;
pub mod irc_handler;
pub mod log_store;
pub mod ui;

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyEventKind};
use futures::StreamExt;
use irc::client::prelude::{Capability, Client, Config as IrcConfig};
use irc::error::Error as IrcError;
use irc::proto::{Command, Response};
use ratatui::DefaultTerminal;
use tokio::sync::{mpsc, watch, Notify};

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
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let mut server_handles = Vec::new();
    for (server_id, srv_cfg) in app_config.enabled_servers() {
        let server_id = server_id.clone();
        let irc_cfg = srv_cfg.to_irc_config(app_config.default_nickname.as_deref())?;
        let tx = tx.clone();
        let shutdown_rx = shutdown_rx.clone();
        let reconnect = app
            .reconnect_notify_for(&server_id)
            .unwrap_or_else(|| Arc::new(Notify::new()));
        server_handles.push(tokio::spawn(run_server_loop(
            server_id,
            irc_cfg,
            tx,
            shutdown_rx,
            reconnect,
        )));
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
                        app.quit_all(None);
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

    let _ = shutdown_tx.send(true);
    for handle in server_handles {
        let _ = handle.await;
    }

    Ok(())
}

enum SessionEnd {
    ReceiverClosed,
    Shutdown,
    Reconnect,
    Retry(String),
}

async fn run_server_loop(
    server_id: String,
    irc_cfg: IrcConfig,
    tx: mpsc::UnboundedSender<ServerEvent>,
    mut shutdown: watch::Receiver<bool>,
    reconnect: Arc<Notify>,
) {
    loop {
        if *shutdown.borrow() {
            break;
        }
        match connect_and_stream(&server_id, irc_cfg.clone(), &tx, &mut shutdown, &reconnect).await
        {
            SessionEnd::ReceiverClosed | SessionEnd::Shutdown => break,
            SessionEnd::Reconnect => continue,
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
        tokio::select! {
            _ = shutdown.changed() => break,
            _ = reconnect.notified() => {}
            _ = tokio::time::sleep(Duration::from_secs(5)) => {}
        }
    }
}

async fn connect_and_stream(
    server_id: &str,
    irc_cfg: IrcConfig,
    tx: &mpsc::UnboundedSender<ServerEvent>,
    shutdown: &mut watch::Receiver<bool>,
    reconnect: &Notify,
) -> SessionEnd {
    if *shutdown.borrow() {
        return SessionEnd::Shutdown;
    }

    let mut client = tokio::select! {
        _ = shutdown.changed() => return SessionEnd::Shutdown,
        _ = reconnect.notified() => return SessionEnd::Reconnect,
        res = Client::from_config(irc_cfg) => match res {
            Ok(c) => c,
            Err(err) => return SessionEnd::Retry(format!("Connection failed: {err} (retrying in 5s)")),
        }
    };

    if *shutdown.borrow() {
        return SessionEnd::Shutdown;
    }

    let _ = client.send_cap_req(&[Capability::MultiPrefix]);
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

    loop {
        if *shutdown.borrow() {
            let _ = tokio::time::timeout(Duration::from_millis(300), stream.next()).await;
            return SessionEnd::Shutdown;
        }

        tokio::select! {
            _ = shutdown.changed() => {
                let _ = tokio::time::timeout(Duration::from_millis(300), stream.next()).await;
                return SessionEnd::Shutdown;
            }
            _ = reconnect.notified() => {
                let _ = tokio::time::timeout(Duration::from_millis(100), stream.next()).await;
                return SessionEnd::Reconnect;
            }
            item = stream.next() => {
                match item {
                    Some(Ok(message)) => {
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
                    Some(Err(IrcError::NoUsableNick)) => {
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
                    Some(Err(err)) => {
                        return SessionEnd::Retry(format!("Disconnected: {err} (reconnecting in 5s)"));
                    }
                    None => {
                        return SessionEnd::Retry("Connection closed (reconnecting in 5s)".to_string());
                    }
                }
            }
        }
    }
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

        let (_shutdown_tx, mut shutdown_rx) = watch::channel(false);
        let reconnect = Notify::new();
        let end =
            connect_and_stream("libera", irc_cfg, &tx, &mut shutdown_rx, &reconnect).await;
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

    #[tokio::test]
    async fn flushes_quit_message_on_shutdown() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let server_thread = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            use std::io::{BufRead, BufReader, Write};
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut received = Vec::new();
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap() > 0 {
                received.push(line.clone());
                if line.starts_with("NICK ") {
                    let _ = socket.write_all(b":mock.server 001 irc_user :Welcome\r\n");
                    let _ = socket.flush();
                }
                if line.starts_with("QUIT") {
                    break;
                }
                line.clear();
            }
            received
        });

        let toml = format!(
            r##"
            default_nickname = "irc_user"
            [servers.test_srv]
            server = "127.0.0.1"
            port = {port}
            use_tls = false
        "##
        );
        let cfg = AppConfig::from_toml_str(&toml).unwrap();
        let mut app = App::from_config(&cfg);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let (srv_id, srv_cfg) = cfg.enabled_servers().next().unwrap();
        let irc_cfg = srv_cfg
            .to_irc_config(cfg.default_nickname.as_deref())
            .unwrap();

        let reconnect = app.reconnect_notify_for(srv_id).unwrap();
        let srv_loop = tokio::spawn(run_server_loop(
            srv_id.clone(),
            irc_cfg,
            tx,
            shutdown_rx,
            reconnect,
        ));

        while let Some(event) = rx.recv().await {
            let is_connected = matches!(event, ServerEvent::Connected { .. });
            app.handle_server_event(event);
            if is_connected {
                break;
            }
        }

        app.quit_all(Some("time to go".into()));
        assert!(app.should_quit);

        let _ = shutdown_tx.send(true);
        let _ = srv_loop.await;

        let received = server_thread.join().unwrap();
        assert!(
            received
                .iter()
                .any(|l| l.contains("CAP REQ") && l.contains("multi-prefix")),
            "Expected multi-prefix capability request in received lines: {received:?}"
        );
        assert!(
            received.iter().any(|l| l.contains("QUIT :time to go")),
            "Expected QUIT message in received lines: {received:?}"
        );
    }

    #[tokio::test]
    async fn reconnect_command_aborts_hung_session_immediately() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let server_thread = std::thread::spawn(move || {
            // First connection hangs without closing after QUIT
            let (_sock1, _) = listener.accept().unwrap();
            // Second connection proves immediate reconnect without 5s backoff
            let (_sock2, _) = listener.accept().unwrap();
        });

        let toml = format!(
            r##"
            default_nickname = "irc_user"
            [servers.test_srv]
            server = "127.0.0.1"
            port = {port}
            use_tls = false
        "##
        );
        let cfg = AppConfig::from_toml_str(&toml).unwrap();
        let mut app = App::from_config(&cfg);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let (srv_id, srv_cfg) = cfg.enabled_servers().next().unwrap();
        let irc_cfg = srv_cfg
            .to_irc_config(cfg.default_nickname.as_deref())
            .unwrap();
        let reconnect = app.reconnect_notify_for(srv_id).unwrap();

        let srv_loop = tokio::spawn(run_server_loop(
            srv_id.clone(),
            irc_cfg,
            tx,
            shutdown_rx,
            reconnect,
        ));

        while let Some(event) = rx.recv().await {
            let is_connected = matches!(event, ServerEvent::Connected { .. });
            app.handle_server_event(event);
            if is_connected {
                break;
            }
        }

        app.execute_user_command(crate::command::UserCommand::Reconnect);

        let reconnected = tokio::time::timeout(Duration::from_secs(2), async {
            while let Some(event) = rx.recv().await {
                if matches!(event, ServerEvent::Connected { .. }) {
                    return true;
                }
            }
            false
        })
        .await
        .unwrap_or(false);

        assert!(reconnected, "Expected immediate reconnect on /reconnect");

        let _ = shutdown_tx.send(true);
        let _ = srv_loop.await;
        server_thread.join().unwrap();
    }

    #[tokio::test]
    async fn ignored_user_does_not_trigger_ctcp_auto_reply() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let server_thread = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            use std::io::{BufRead, BufReader, Write};
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut received = Vec::new();
            let mut line = String::new();
            let mut welcomed = false;
            while reader.read_line(&mut line).unwrap() > 0 {
                received.push(line.clone());
                if !welcomed && line.starts_with("USER ") {
                    welcomed = true;
                    let _ = socket.write_all(
                        b":mock.server 001 irc_user :Welcome\r\n\
                          :ignored_bot!b@h PRIVMSG irc_user :\x01VERSION\x01\r\n\
                          :friend!f@h PRIVMSG irc_user :\x01PING 42\x01\r\n",
                    );
                    let _ = socket.flush();
                }
                if line.contains("NOTICE friend :\x01PING 42\x01") {
                    break;
                }
                line.clear();
            }
            received
        });

        let toml = format!(
            r##"
            default_nickname = "irc_user"
            [servers.test_srv]
            server = "127.0.0.1"
            port = {port}
            use_tls = false
        "##
        );
        let cfg = AppConfig::from_toml_str(&toml).unwrap();
        let mut app = App::from_config(&cfg);
        app.execute_user_command(crate::command::UserCommand::Ignore(Some(
            "ignored_bot".into(),
        )));

        let (tx, mut rx) = mpsc::unbounded_channel();
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (srv_id, srv_cfg) = cfg.enabled_servers().next().unwrap();
        let irc_cfg = srv_cfg
            .to_irc_config(cfg.default_nickname.as_deref())
            .unwrap();
        let reconnect = app.reconnect_notify_for(srv_id).unwrap();

        let srv_loop = tokio::spawn(run_server_loop(
            srv_id.clone(),
            irc_cfg,
            tx,
            shutdown_rx,
            reconnect,
        ));

        tokio::time::timeout(Duration::from_secs(2), async {
            while let Some(ev) = rx.recv().await {
                let is_friend_ping = matches!(
                    &ev,
                    ServerEvent::Message { message, .. }
                        if matches!(&message.command, Command::PRIVMSG(_, t) if t.contains("PING"))
                );
                app.handle_server_event(ev);
                if is_friend_ping {
                    break;
                }
            }
        })
        .await
        .unwrap();

        let _ = shutdown_tx.send(true);
        let _ = srv_loop.await;
        let received = server_thread.join().unwrap();

        assert!(
            !received.iter().any(|l| l.contains("NOTICE ignored_bot")),
            "Ignored user must not receive CTCP reply: {received:?}"
        );
        assert!(
            received
                .iter()
                .any(|l| l.contains("NOTICE friend :\x01PING 42\x01")),
            "Non-ignored user should receive CTCP PING reply: {received:?}"
        );
    }
}
