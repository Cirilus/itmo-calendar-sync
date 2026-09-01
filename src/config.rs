use std::{env, net::SocketAddr, time::Duration};

use crate::{AnyError, any_error};

#[derive(Clone)]
pub struct TelegramConfig {
    pub bot_token: String,
    pub chat_id: String,
    pub proxy_url: Option<String>,
    pub silent: bool,
}

#[derive(Clone)]
pub struct Config {
    pub listen_addr: SocketAddr,
    pub calendar_token: String,
    pub itmo_username: String,
    pub itmo_password: String,
    pub cache_ttl: Duration,
    pub telegram: Option<TelegramConfig>,
}

impl Config {
    pub fn from_env() -> Result<Self, AnyError> {
        let listen_addr = env::var("LISTEN_ADDR")
            .unwrap_or_else(|_| "0.0.0.0:3000".to_owned())
            .parse()
            .map_err(|error| any_error(format!("invalid LISTEN_ADDR: {error}")))?;

        let calendar_token = required("CALENDAR_TOKEN")?;
        if calendar_token.len() < 32 {
            return Err(any_error(
                "CALENDAR_TOKEN must contain at least 32 characters",
            ));
        }

        let cache_ttl_seconds = env::var("CACHE_TTL_SECONDS")
            .unwrap_or_else(|_| "900".to_owned())
            .parse::<u64>()
            .map_err(|error| any_error(format!("invalid CACHE_TTL_SECONDS: {error}")))?;

        if cache_ttl_seconds == 0 {
            return Err(any_error("CACHE_TTL_SECONDS must be greater than zero"));
        }

        let bot_token = optional("TELEGRAM_BOT_TOKEN");
        let chat_id = optional("TELEGRAM_CHAT_ID");
        let telegram = match (bot_token, chat_id) {
            (None, None) => None,
            (Some(bot_token), Some(chat_id)) => Some(TelegramConfig {
                bot_token,
                chat_id,
                proxy_url: optional("TELEGRAM_PROXY_URL"),
                silent: parse_bool("TELEGRAM_SILENT", false)?,
            }),
            _ => {
                return Err(any_error(
                    "TELEGRAM_BOT_TOKEN and TELEGRAM_CHAT_ID must be set together",
                ));
            }
        };

        let (itmo_username, itmo_password) = Self::itmo_credentials_from_env()?;

        Ok(Self {
            listen_addr,
            calendar_token,
            itmo_username,
            itmo_password,
            cache_ttl: Duration::from_secs(cache_ttl_seconds),
            telegram,
        })
    }

    pub fn itmo_credentials_from_env() -> Result<(String, String), AnyError> {
        Ok((required("ITMO_USERNAME")?, required("ITMO_PASSWORD")?))
    }
}

fn required(name: &str) -> Result<String, AnyError> {
    optional(name).ok_or_else(|| any_error(format!("{name} is required")))
}

fn optional(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn parse_bool(name: &str, default: bool) -> Result<bool, AnyError> {
    let Some(value) = optional(name) else {
        return Ok(default);
    };

    match value.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        _ => Err(any_error(format!("{name} must be true or false"))),
    }
}
