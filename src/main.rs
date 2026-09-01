mod auth;
mod cache;
mod calendar;
mod config;
mod itmo;
mod telegram;

use std::{io, sync::Arc};

use axum::{
    Router,
    extract::{Path, State},
    http::{
        HeaderName, HeaderValue, StatusCode,
        header::{CACHE_CONTROL, CONTENT_TYPE},
    },
    response::{IntoResponse, Response},
    routing::get,
};
use cache::CalendarService;
use config::Config;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use tracing_subscriber::EnvFilter;

pub type AnyError = Box<dyn std::error::Error + Send + Sync>;

pub fn any_error(message: impl Into<String>) -> AnyError {
    Box::new(io::Error::other(message.into()))
}

struct AppState {
    calendar_token: String,
    service: CalendarService,
}

#[tokio::main]
async fn main() -> Result<(), AnyError> {
    let mut arguments = std::env::args().skip(1);
    let command = arguments.next();

    match command.as_deref() {
        Some("healthcheck") => process_healthcheck().await,
        Some("fetch-api") => {
            init_tracing();
            let output = arguments
                .next()
                .unwrap_or_else(|| "itmo-api.json".to_owned());
            process_fetch_api(&output).await
        }
        Some("convert-ics") => {
            init_tracing();
            let input = arguments
                .next()
                .unwrap_or_else(|| "itmo-api.json".to_owned());
            let output = arguments
                .next()
                .unwrap_or_else(|| "schedule.ics".to_owned());
            process_convert_ics(&input, &output)
        }
        Some(command) => Err(any_error(format!("unknown command: {command}"))),
        None => {
            init_tracing();
            run_server().await
        }
    }
}

fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
}

async fn run_server() -> Result<(), AnyError> {
    let config = Config::from_env()?;
    let listen_addr = config.listen_addr;
    let state = Arc::new(AppState {
        calendar_token: config.calendar_token.clone(),
        service: CalendarService::new(&config)?,
    });

    let app = Router::new()
        .route("/health", get(health))
        .route("/calendar/{calendar_file}", get(calendar))
        .with_state(state);

    let listener = TcpListener::bind(listen_addr).await?;
    tracing::info!(%listen_addr, "ITMO Calendar Sync started");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

async fn process_fetch_api(output: &str) -> Result<(), AnyError> {
    let (username, password) = Config::itmo_credentials_from_env()?;
    let auth = Arc::new(auth::AuthClient::new(username, password)?);
    let client = itmo::ItmoClient::new()?;
    let response = client.fetch_schedule_raw(&auth).await?;
    let days = itmo::parse_schedule_value(response.clone())?;
    let lessons = days.iter().map(|day| day.lessons.len()).sum::<usize>();
    let json = format!("{}\n", serde_json::to_string_pretty(&response)?);

    std::fs::write(output, json)?;
    println!(
        "Ответ API сохранён в {output}: {} дней, {lessons} занятий",
        days.len()
    );
    Ok(())
}

fn process_convert_ics(input: &str, output: &str) -> Result<(), AnyError> {
    let json = std::fs::read_to_string(input)?;
    let days = itmo::parse_schedule_json(&json)?;
    let lessons = days.iter().map(|day| day.lessons.len()).sum::<usize>();
    let ics = calendar::build_calendar(&days)?;

    std::fs::write(output, ics)?;
    println!(
        "ICS сохранён в {output}: {} дней, {lessons} занятий",
        days.len()
    );
    Ok(())
}

async fn health() -> &'static str {
    "ok"
}

async fn calendar(
    State(state): State<Arc<AppState>>,
    Path(calendar_file): Path<String>,
) -> Response {
    let Some(token) = calendar_file.strip_suffix(".ics") else {
        return StatusCode::NOT_FOUND.into_response();
    };

    if !constant_time_eq(token.as_bytes(), state.calendar_token.as_bytes()) {
        return StatusCode::NOT_FOUND.into_response();
    }

    match state.service.calendar().await {
        Ok(payload) => {
            let mut response = (StatusCode::OK, payload.body).into_response();
            let headers = response.headers_mut();
            headers.insert(
                CONTENT_TYPE,
                HeaderValue::from_static("text/calendar; charset=utf-8"),
            );
            headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));

            if payload.stale {
                headers.insert(
                    HeaderName::from_static("warning"),
                    HeaderValue::from_static("110 - \"Response is stale\""),
                );
            }

            response
        }
        Err(error) => {
            tracing::error!(error = %error, "calendar request cannot be served");
            (
                StatusCode::BAD_GATEWAY,
                "Schedule is temporarily unavailable",
            )
                .into_response()
        }
    }
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let max_len = left.len().max(right.len());
    let mut difference = left.len() ^ right.len();

    for index in 0..max_len {
        let left_byte = left.get(index).copied().unwrap_or(0);
        let right_byte = right.get(index).copied().unwrap_or(0);
        difference |= usize::from(left_byte ^ right_byte);
    }

    difference == 0
}

async fn process_healthcheck() -> Result<(), AnyError> {
    let mut stream = TcpStream::connect("127.0.0.1:3000").await?;
    stream
        .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await?;

    let mut response = [0_u8; 256];
    let bytes_read = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        stream.read(&mut response),
    )
    .await
    .map_err(|_| any_error("healthcheck timed out"))??;

    let response = String::from_utf8_lossy(&response[..bytes_read]);
    if response.starts_with("HTTP/1.1 200") {
        Ok(())
    } else {
        Err(any_error("healthcheck returned a non-200 response"))
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::error!(%error, "could not install Ctrl+C handler");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                tracing::error!(%error, "could not install termination handler");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
