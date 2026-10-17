// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

use anyhow::{bail, Context, Result};
use irc::client::prelude::Config as IrcConfig;
use serde::Deserialize;

pub const DEFAULT_GENERIC_NICK: &str = "irc_user";

#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    pub default_nickname: Option<String>,
    pub servers: BTreeMap<String, ServerConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    pub nickname: Option<String>,
    pub server: String,
    pub port: Option<u16>,
    pub use_tls: Option<bool>,
    #[serde(default)]
    pub channels: Vec<String>,
    pub password: Option<String>,
    pub password_command: Option<String>,
    pub nick_password: Option<String>,
    pub nick_password_command: Option<String>,
    pub client_cert_path: Option<String>,
    pub enabled: Option<bool>,
}

impl ServerConfig {
    pub fn is_enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }

    pub fn effective_nickname<'a>(&'a self, fallback_nick: Option<&'a str>) -> Result<String> {
        let nick = self
            .nickname
            .as_deref()
            .or(fallback_nick)
            .map(str::trim)
            .filter(|s| !s.is_empty());

        match nick {
            Some(n) => Ok(n.to_string()),
            None => bail!(
                "No nickname configured for server '{}' (set `nickname` or `default_nickname`)",
                self.server
            ),
        }
    }

    pub fn effective_tls(&self) -> bool {
        self.use_tls.unwrap_or(true)
    }

    pub fn effective_port(&self) -> u16 {
        self.port
            .unwrap_or_else(|| if self.effective_tls() { 6697 } else { 6667 })
    }

    pub fn resolve_password(&self) -> Result<Option<String>> {
        if let Some(cmd) = &self.password_command {
            return Ok(Some(run_secret_command(cmd)?));
        }
        Ok(self.password.clone())
    }

    pub fn resolve_nick_password(&self) -> Result<Option<String>> {
        if let Some(cmd) = &self.nick_password_command {
            return Ok(Some(run_secret_command(cmd)?));
        }
        Ok(self.nick_password.clone())
    }

    pub fn to_irc_config(&self, fallback_nick: Option<&str>) -> Result<IrcConfig> {
        if self.server.trim().is_empty() {
            bail!("Server hostname cannot be empty");
        }
        let nickname = self.effective_nickname(fallback_nick)?;
        let use_tls = self.effective_tls();
        let port = self.effective_port();
        let password = self.resolve_password()?;
        let nick_password = self.resolve_nick_password()?;

        Ok(IrcConfig {
            nickname: Some(nickname.clone()),
            username: Some(nickname.clone()),
            realname: Some(nickname),
            server: Some(self.server.clone()),
            port: Some(port),
            use_tls: Some(use_tls),
            channels: self.channels.clone(),
            password,
            nick_password,
            client_cert_path: self.client_cert_path.clone(),
            ..IrcConfig::default()
        })
    }
}

fn run_secret_command(cmd: &str) -> Result<String> {
    let output = ProcessCommand::new("sh")
        .arg("-c")
        .arg(cmd)
        .output()
        .with_context(|| format!("Failed to execute secret command '{cmd}'"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "Secret command '{cmd}' exited with {}: {}",
            output.status,
            stderr.trim()
        );
    }

    let secret = String::from_utf8(output.stdout)
        .context("Secret command output is not valid UTF-8")?
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string();

    if secret.is_empty() {
        bail!("Secret command '{cmd}' returned empty output");
    }

    Ok(secret)
}

impl AppConfig {
    /// Resolves the configuration path from:
    /// 1. Explicit CLI argument if provided
    /// 2. `~/.config/ronf/config.toml` (or `$XDG_CONFIG_HOME/ronf/config.toml`) if it exists
    /// 3. `./config.toml` in the current working directory
    pub fn resolve_path(cli_arg: Option<&str>) -> PathBuf {
        if let Some(arg) = cli_arg {
            return PathBuf::from(arg);
        }

        if let Some(user_cfg) = Self::user_config_path() {
            if user_cfg.exists() {
                return user_cfg;
            }
        }

        PathBuf::from("config.toml")
    }

    pub fn user_config_path() -> Option<PathBuf> {
        if let Ok(xdg) = env::var("XDG_CONFIG_HOME") {
            let trimmed = xdg.trim();
            if !trimmed.is_empty() {
                return Some(PathBuf::from(trimmed).join("ronf").join("config.toml"));
            }
        }
        env::var("HOME")
            .ok()
            .filter(|h| !h.trim().is_empty())
            .map(|home| PathBuf::from(home).join(".config").join("ronf").join("config.toml"))
    }

    pub fn from_file(path: impl AsRef<Path>) -> Result<Self> {
        let path_ref = path.as_ref();
        let raw = fs::read_to_string(path_ref)
            .with_context(|| format!("Failed to read config file '{}'", path_ref.display()))?;
        Self::from_toml_str(&raw)
    }

    pub fn from_toml_str(raw: &str) -> Result<Self> {
        let cfg: Self = toml::from_str(raw).context("Failed to parse TOML configuration")?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn enabled_servers(&self) -> impl Iterator<Item = (&String, &ServerConfig)> {
        self.servers.iter().filter(|(_, s)| s.is_enabled())
    }

    pub fn validate(&self) -> Result<()> {
        let mut enabled_count = 0usize;
        for (id, srv) in &self.servers {
            if !srv.is_enabled() {
                continue;
            }
            enabled_count += 1;
            srv.to_irc_config(self.default_nickname.as_deref())
                .with_context(|| format!("Invalid config for [servers.{id}]"))?;
        }

        if enabled_count == 0 {
            bail!("Configuration must contain at least one enabled [servers.<name>] section");
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_multi_server_config_with_inheritance_and_defaults() {
        let toml = r##"
            default_nickname = "irc_user"

            [servers.libera]
            server = "irc.libera.chat"
            channels = ["#linux", "#rust"]

            [servers.oftc]
            nickname = "oftc_user"
            server = "irc.oftc.net"
            use_tls = false

            [servers.disabled]
            enabled = false
            server = "127.0.0.1"
        "##;

        let cfg = AppConfig::from_toml_str(toml).expect("valid config");
        let enabled: Vec<_> = cfg.enabled_servers().collect();
        assert_eq!(enabled.len(), 2);

        let libera = &cfg.servers["libera"];
        let libera_irc = libera
            .to_irc_config(cfg.default_nickname.as_deref())
            .unwrap();
        assert_eq!(libera_irc.nickname.as_deref(), Some("irc_user"));
        assert_eq!(libera_irc.port, Some(6697));
        assert_eq!(libera_irc.use_tls, Some(true));
        assert_eq!(libera_irc.channels, vec!["#linux", "#rust"]);

        let oftc = &cfg.servers["oftc"];
        let oftc_irc = oftc.to_irc_config(cfg.default_nickname.as_deref()).unwrap();
        assert_eq!(oftc_irc.nickname.as_deref(), Some("oftc_user"));
        assert_eq!(oftc_irc.port, Some(6667));
        assert_eq!(oftc_irc.use_tls, Some(false));
    }

    #[test]
    fn rejects_missing_nickname_and_no_enabled_servers() {
        let missing_nick = r#"
            [servers.libera]
            server = "irc.libera.chat"
        "#;
        assert!(AppConfig::from_toml_str(missing_nick).is_err());

        let no_enabled = r#"
            default_nickname = "irc_user"
            [servers.local]
            enabled = false
            server = "127.0.0.1"
        "#;
        assert!(AppConfig::from_toml_str(no_enabled).is_err());
    }

    #[test]
    fn example_repo_config_toml_is_valid() {
        let cfg = AppConfig::from_file("config.toml").expect("example config.toml should parse");
        assert_eq!(cfg.default_nickname.as_deref(), Some("irc_user"));
        let libera = &cfg.servers["libera"];
        assert_eq!(libera.channels, vec!["#linux", "#rust"]);
    }

    #[test]
    fn resolves_password_command_and_cert_path() {
        let toml = r##"
            default_nickname = "irc_user"

            [servers.libera]
            server = "irc.libera.chat"
            password_command = "printf 'my_secret_pw\n'"
            nick_password = "nickserv_pw"
            client_cert_path = "/tmp/client.pem"
        "##;

        let cfg = AppConfig::from_toml_str(toml).expect("valid config");
        let irc_cfg = cfg.servers["libera"]
            .to_irc_config(cfg.default_nickname.as_deref())
            .unwrap();
        assert_eq!(irc_cfg.password.as_deref(), Some("my_secret_pw"));
        assert_eq!(irc_cfg.nick_password.as_deref(), Some("nickserv_pw"));
        assert_eq!(irc_cfg.client_cert_path.as_deref(), Some("/tmp/client.pem"));
    }
}
