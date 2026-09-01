use std::sync::Arc;

use chrono::{Datelike, FixedOffset, NaiveDate, TimeZone, Utc};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Deserializer};
use serde_json::Value;

use crate::{AnyError, any_error, auth::AuthClient};

const SCHEDULE_URL: &str = "https://my.itmo.ru/api/schedule/schedule/personal";

#[derive(Clone)]
pub struct ItmoClient {
    http: Client,
}

impl ItmoClient {
    pub fn new() -> Result<Self, AnyError> {
        Ok(Self {
            http: Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .user_agent("itmo-calendar-sync/0.1")
                .build()?,
        })
    }

    pub async fn fetch_schedule(
        &self,
        auth: &Arc<AuthClient>,
    ) -> Result<Vec<ScheduleDay>, AnyError> {
        parse_schedule_value(self.fetch_schedule_raw(auth).await?)
    }

    pub async fn fetch_schedule_raw(&self, auth: &Arc<AuthClient>) -> Result<Value, AnyError> {
        let (start, end) = academic_year_range()?;

        for attempt in 0..2 {
            let token = auth.access_token().await?;
            let response = self
                .http
                .get(SCHEDULE_URL)
                .bearer_auth(token)
                .query(&[
                    ("date_start", start.format("%Y-%m-%d").to_string()),
                    ("date_end", end.format("%Y-%m-%d").to_string()),
                ])
                .send()
                .await?;

            if response.status() == StatusCode::UNAUTHORIZED && attempt == 0 {
                auth.invalidate().await;
                continue;
            }

            if !response.status().is_success() {
                return Err(any_error(format!(
                    "ITMO schedule endpoint returned {}",
                    response.status()
                )));
            }

            return Ok(response.json::<Value>().await?);
        }

        Err(any_error("ITMO authorization failed after token refresh"))
    }
}

pub fn parse_schedule_json(source: &str) -> Result<Vec<ScheduleDay>, AnyError> {
    parse_schedule_value(serde_json::from_str(source)?)
}

pub fn parse_schedule_value(value: Value) -> Result<Vec<ScheduleDay>, AnyError> {
    Ok(serde_json::from_value::<ScheduleResponse>(value)?.data)
}

#[derive(Deserialize)]
struct ScheduleResponse {
    #[serde(default)]
    data: Vec<ScheduleDay>,
}

#[derive(Debug, Deserialize)]
pub struct ScheduleDay {
    #[serde(default, deserialize_with = "string_value")]
    pub date: String,
    #[serde(default)]
    pub lessons: Vec<Lesson>,
}

#[derive(Debug, Deserialize)]
pub struct Lesson {
    #[serde(default, deserialize_with = "optional_string")]
    pub id: Option<String>,
    #[serde(default, deserialize_with = "string_value")]
    pub subject: String,
    #[serde(default, rename = "type", deserialize_with = "optional_string")]
    pub lesson_type: Option<String>,
    #[serde(default, deserialize_with = "string_value")]
    pub time_start: String,
    #[serde(default, deserialize_with = "string_value")]
    pub time_end: String,
    #[serde(default, deserialize_with = "optional_string")]
    pub building: Option<String>,
    #[serde(default, deserialize_with = "optional_string")]
    pub room: Option<String>,
    #[serde(default, deserialize_with = "optional_string")]
    pub teacher_name: Option<String>,
    #[serde(default, deserialize_with = "optional_string")]
    pub group: Option<String>,
    #[serde(default, deserialize_with = "optional_string")]
    pub note: Option<String>,
}

fn academic_year_range() -> Result<(NaiveDate, NaiveDate), AnyError> {
    let moscow = FixedOffset::east_opt(3 * 60 * 60)
        .ok_or_else(|| any_error("cannot construct Moscow timezone"))?;
    let today = Utc::now().with_timezone(&moscow).date_naive();
    let start_year = if today.month() >= 8 {
        today.year()
    } else {
        today.year() - 1
    };

    let start = moscow
        .with_ymd_and_hms(start_year, 8, 1, 0, 0, 0)
        .single()
        .ok_or_else(|| any_error("cannot calculate academic year start"))?
        .date_naive();
    let end = moscow
        .with_ymd_and_hms(start_year + 1, 7, 31, 0, 0, 0)
        .single()
        .ok_or_else(|| any_error("cannot calculate academic year end"))?
        .date_naive();

    Ok((start, end))
}

fn string_value<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(value_to_string(Option::<Value>::deserialize(deserializer)?).unwrap_or_default())
}

fn optional_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(value_to_string(Option::<Value>::deserialize(deserializer)?))
}

fn value_to_string(value: Option<Value>) -> Option<String> {
    match value? {
        Value::Null => None,
        Value::String(value) => non_empty(value),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        Value::Array(values) => {
            let joined = values
                .into_iter()
                .filter_map(|value| value_to_string(Some(value)))
                .collect::<Vec<_>>()
                .join(", ");
            non_empty(joined)
        }
        Value::Object(_) => None,
    }
}

fn non_empty(value: String) -> Option<String> {
    let value = value.trim().to_owned();
    (!value.is_empty()).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::parse_schedule_json;

    #[test]
    fn parses_schedule_api_response() {
        let source = r#"{
            "data": [{
                "date": "2026-09-01",
                "lessons": [{
                    "id": 42,
                    "subject": "Математика",
                    "type": "Лекция",
                    "time_start": "09:00",
                    "time_end": "10:30",
                    "building": "Кронверкский пр., 49",
                    "room": "101",
                    "teacher_name": "Иванов И. И.",
                    "group": "M0000",
                    "note": null
                }]
            }]
        }"#;

        let days = parse_schedule_json(source).expect("schedule must be parsed");

        assert_eq!(days.len(), 1);
        assert_eq!(days[0].lessons.len(), 1);
        assert_eq!(days[0].lessons[0].id.as_deref(), Some("42"));
        assert_eq!(days[0].lessons[0].subject, "Математика");
    }
}
