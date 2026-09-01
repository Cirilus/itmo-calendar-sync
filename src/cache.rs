use std::{sync::Arc, time::Instant};

use tokio::sync::Mutex;

use crate::{
    AnyError, any_error, auth::AuthClient, calendar, config::Config, itmo::ItmoClient,
    telegram::TelegramNotifier,
};

struct CacheEntry {
    body: String,
    fetched_at: Instant,
}

pub struct CalendarPayload {
    pub body: String,
    pub stale: bool,
}

pub struct CalendarService {
    auth: Arc<AuthClient>,
    itmo: ItmoClient,
    notifier: TelegramNotifier,
    cache_ttl: std::time::Duration,
    cache: Mutex<Option<CacheEntry>>,
    refresh_lock: Mutex<()>,
}

impl CalendarService {
    pub fn new(config: &Config) -> Result<Self, AnyError> {
        Ok(Self {
            auth: Arc::new(AuthClient::new(
                config.itmo_username.clone(),
                config.itmo_password.clone(),
            )?),
            itmo: ItmoClient::new()?,
            notifier: TelegramNotifier::new(config.telegram.clone())?,
            cache_ttl: config.cache_ttl,
            cache: Mutex::new(None),
            refresh_lock: Mutex::new(()),
        })
    }

    pub async fn calendar(&self) -> Result<CalendarPayload, AnyError> {
        if let Some(body) = self.fresh_body().await {
            return Ok(CalendarPayload { body, stale: false });
        }

        let _refresh_guard = self.refresh_lock.lock().await;

        if let Some(body) = self.fresh_body().await {
            return Ok(CalendarPayload { body, stale: false });
        }

        match self.refresh().await {
            Ok(body) => {
                *self.cache.lock().await = Some(CacheEntry {
                    body: body.clone(),
                    fetched_at: Instant::now(),
                });
                self.notifier.report_recovery().await;
                tracing::info!("ITMO schedule cache refreshed");
                Ok(CalendarPayload { body, stale: false })
            }
            Err(error) => {
                tracing::error!(error = %error, "ITMO schedule refresh failed");
                self.notifier.report_failure(&error.to_string()).await;

                if let Some(body) = self.stale_body().await {
                    Ok(CalendarPayload { body, stale: true })
                } else {
                    Err(any_error("schedule is temporarily unavailable"))
                }
            }
        }
    }

    async fn refresh(&self) -> Result<String, AnyError> {
        let days = self.itmo.fetch_schedule(&self.auth).await?;
        calendar::build_calendar(&days)
    }

    async fn fresh_body(&self) -> Option<String> {
        let cache = self.cache.lock().await;
        cache.as_ref().and_then(|entry| {
            (entry.fetched_at.elapsed() < self.cache_ttl).then(|| entry.body.clone())
        })
    }

    async fn stale_body(&self) -> Option<String> {
        self.cache
            .lock()
            .await
            .as_ref()
            .map(|entry| entry.body.clone())
    }
}
