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
pub const DEFAULT_HISTORY_LIMIT: usize = 100;

#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    pub default_nickname: Option<String>,
    pub enable_logging: Option<bool>,
    pub log_dir: Option<String>,
    pub history_limit: Option<usize>,
    pub show_join_part: Option<bool>,
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
    pub history_limit: Option<usize>,
    pub show_join_part: Option<bool>,
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

    pub fn effective_show_join_part(&self, global_default: Option<bool>) -> bool {
        self.show_join_part.or(global_default).unwrap_or(true)
    }

    pub fn effective_history_limit(&self, global_default: Option<usize>) -> usize {
        self.history_limit
            .or(global_default)
            .unwrap_or(DEFAULT_HISTORY_LIMIT)
    }

    pub fn effective_port(&self) -> u16 {
        self.port
            .unwrap_or_else(|| if self.effective_tls() { 6697 } else { 6667 })
    }

    pub fn resolve_password(&self) -> Result<Option<String>> {
        resolve_secret(&self.password, &self.password_command)
    }

    pub fn resolve_nick_password(&self) -> Result<Option<String>> {
        resolve_secret(&self.nick_password, &self.nick_password_command)
    }

    fn validated_nickname(&self, fallback_nick: Option<&str>) -> Result<String> {
        if self.server.trim().is_empty() {
            bail!("Server hostname cannot be empty");
        }
        self.effective_nickname(fallback_nick)
    }

    pub fn validate_structure(&self, fallback_nick: Option<&str>) -> Result<()> {
        self.validated_nickname(fallback_nick).map(|_| ())
    }

    pub fn to_irc_config(&self, fallback_nick: Option<&str>) -> Result<IrcConfig> {
        let nickname = self.validated_nickname(fallback_nick)?;
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
            channels: Vec::new(),
            password,
            nick_password,
            client_cert_path: self.client_cert_path.clone(),
            ..IrcConfig::default()
        })
    }
}

fn resolve_secret(plain: &Option<String>, cmd: &Option<String>) -> Result<Option<String>> {
    match cmd {
        Some(c) => Ok(Some(run_secret_command(c)?)),
        None => Ok(plain.clone()),
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
        xdg_path("XDG_CONFIG_HOME", &[".config"], &["ronf", "config.toml"])
    }

    pub fn effective_log_dir(&self) -> Option<PathBuf> {
        if !self.enable_logging.unwrap_or(true) {
            return None;
        }
        if let Some(dir) = &self.log_dir {
            return Some(PathBuf::from(dir));
        }
        #[cfg(test)]
        {
            None
        }
        #[cfg(not(test))]
        {
            Self::default_log_dir()
        }
    }

    pub fn default_log_dir() -> Option<PathBuf> {
        xdg_path("XDG_DATA_HOME", &[".local", "share"], &["ronf", "logs"])
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
            srv.validate_structure(self.default_nickname.as_deref())
                .with_context(|| format!("Invalid config for [servers.{id}]"))?;
        }

        if enabled_count == 0 {
            bail!("Configuration must contain at least one enabled [servers.<name>] section");
        }

        Ok(())
    }
}

fn xdg_path(env_key: &str, home_rel: &[&str], subpath: &[&str]) -> Option<PathBuf> {
    let mut base = env::var(env_key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            let home = env::var("HOME").ok()?;
            let trimmed = home.trim();
            if trimmed.is_empty() {
                return None;
            }
            let mut p = PathBuf::from(trimmed);
            p.extend(home_rel);
            Some(p)
        })?;
    base.extend(subpath);
    Some(base)
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
        assert!(libera_irc.channels.is_empty());
        assert_eq!(libera.channels, vec!["#linux", "#rust"]);

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
        assert_eq!(
            libera.channels,
            vec!["#linux", "#ltp", "#ai", "#c", "##rust", "#archlinux"]
        );
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

    #[test]
    fn resolves_effective_show_join_part_with_override() {
        let toml = r##"
            default_nickname = "irc_user"
            show_join_part = false

            [servers.libera]
            server = "irc.libera.chat"

            [servers.oftc]
            server = "irc.oftc.net"
            show_join_part = true
        "##;

        let cfg = AppConfig::from_toml_str(toml).expect("valid config");
        assert_eq!(cfg.show_join_part, Some(false));
        assert!(!cfg.servers["libera"].effective_show_join_part(cfg.show_join_part));
        assert!(cfg.servers["oftc"].effective_show_join_part(cfg.show_join_part));
    }

    #[test]
    fn resolves_effective_history_limit_and_logging_defaults() {
        let toml = r##"
            default_nickname = "irc_user"
            log_dir = "/tmp/ronf_logs"

            [servers.libera]
            server = "irc.libera.chat"

            [servers.oftc]
            server = "irc.oftc.net"
            history_limit = 250

            [servers.silent]
            server = "irc.example.org"
            history_limit = 0
        "##;

        let cfg = AppConfig::from_toml_str(toml).expect("valid config");
        assert_eq!(
            cfg.effective_log_dir(),
            Some(PathBuf::from("/tmp/ronf_logs"))
        );
        assert_eq!(
            cfg.servers["libera"].effective_history_limit(cfg.history_limit),
            DEFAULT_HISTORY_LIMIT
        );
        assert_eq!(
            cfg.servers["oftc"].effective_history_limit(cfg.history_limit),
            250
        );
        assert_eq!(
            cfg.servers["silent"].effective_history_limit(cfg.history_limit),
            0
        );

        let global_override_toml = r##"
            default_nickname = "irc_user"
            enable_logging = false
            log_dir = "/tmp/ronf_logs"
            history_limit = 50

            [servers.libera]
            server = "irc.libera.chat"

            [servers.oftc]
            server = "irc.oftc.net"
            history_limit = 10
        "##;

        let cfg2 = AppConfig::from_toml_str(global_override_toml).expect("valid config");
        assert_eq!(cfg2.effective_log_dir(), None);
        assert_eq!(
            cfg2.servers["libera"].effective_history_limit(cfg2.history_limit),
            50
        );
        assert_eq!(
            cfg2.servers["oftc"].effective_history_limit(cfg2.history_limit),
            10
        );
    }
}
