use std::sync::atomic::{AtomicBool, Ordering};

use reqwest::{Client, Proxy};
use serde::Serialize;

use crate::{AnyError, any_error, config::TelegramConfig};

pub struct TelegramNotifier {
    client: Option<Client>,
    bot_token: Option<String>,
    chat_id: Option<String>,
    silent: bool,
    had_error: AtomicBool,
}

impl TelegramNotifier {
    pub fn new(config: Option<TelegramConfig>) -> Result<Self, AnyError> {
        let Some(config) = config else {
            return Ok(Self {
                client: None,
                bot_token: None,
                chat_id: None,
                silent: false,
                had_error: AtomicBool::new(false),
            });
        };

        let mut builder = Client::builder().timeout(std::time::Duration::from_secs(20));
        if let Some(proxy_url) = config.proxy_url.as_deref() {
            builder = builder.proxy(Proxy::all(proxy_url)?);
        }

        Ok(Self {
            client: Some(builder.build()?),
            bot_token: Some(config.bot_token),
            chat_id: Some(config.chat_id),
            silent: config.silent,
            had_error: AtomicBool::new(false),
        })
    }

    pub async fn report_failure(&self, error: &str) {
        if self
            .had_error
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }

        let message = format!(
            "🔴 ITMO Calendar Sync: не удалось обновить расписание.\n{}",
            truncate(error, 1500)
        );
        if let Err(send_error) = self.send(&message).await {
            tracing::warn!(error = %send_error, "could not send Telegram failure notification");
        }
    }

    pub async fn report_recovery(&self) {
        if !self.had_error.swap(false, Ordering::AcqRel) {
            return;
        }

        if let Err(send_error) = self
            .send("🟢 ITMO Calendar Sync: обновление расписания снова работает.")
            .await
        {
            tracing::warn!(error = %send_error, "could not send Telegram recovery notification");
        }
    }

    async fn send(&self, text: &str) -> Result<(), AnyError> {
        let (Some(client), Some(bot_token), Some(chat_id)) = (
            self.client.as_ref(),
            self.bot_token.as_deref(),
            self.chat_id.as_deref(),
        ) else {
            return Ok(());
        };

        let endpoint = format!("https://api.telegram.org/bot{bot_token}/sendMessage");
        let response = client
            .post(endpoint)
            .json(&SendMessage {
                chat_id,
                text,
                disable_notification: self.silent,
            })
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(any_error(format!(
                "Telegram Bot API returned {}",
                response.status()
            )));
        }

        Ok(())
    }
}

#[derive(Serialize)]
struct SendMessage<'a> {
    chat_id: &'a str,
    text: &'a str,
    disable_notification: bool,
}

fn truncate(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let prefix = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}
