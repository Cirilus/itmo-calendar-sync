use chrono::{NaiveDate, NaiveDateTime, NaiveTime, Utc};
use sha2::{Digest, Sha256};

use crate::{
    AnyError, any_error,
    itmo::{Lesson, ScheduleDay},
};

pub fn build_calendar(days: &[ScheduleDay]) -> Result<String, AnyError> {
    let now = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let mut output = String::new();

    for line in [
        "BEGIN:VCALENDAR",
        "VERSION:2.0",
        "PRODID:-//ITMO Calendar Sync//RU",
        "CALSCALE:GREGORIAN",
        "METHOD:PUBLISH",
        "X-WR-CALNAME:Расписание ИТМО",
        "X-WR-TIMEZONE:Europe/Moscow",
        "BEGIN:VTIMEZONE",
        "TZID:Europe/Moscow",
        "X-LIC-LOCATION:Europe/Moscow",
        "BEGIN:STANDARD",
        "TZOFFSETFROM:+0300",
        "TZOFFSETTO:+0300",
        "TZNAME:MSK",
        "DTSTART:19700101T000000",
        "END:STANDARD",
        "END:VTIMEZONE",
    ] {
        push_line(&mut output, line);
    }

    for day in days {
        let date = NaiveDate::parse_from_str(&day.date, "%Y-%m-%d").map_err(|_| {
            any_error(format!(
                "ITMO returned an invalid lesson date: {}",
                day.date
            ))
        })?;

        for lesson in &day.lessons {
            append_event(&mut output, date, lesson, &now)?;
        }
    }

    push_line(&mut output, "END:VCALENDAR");
    Ok(output)
}

fn append_event(
    output: &mut String,
    date: NaiveDate,
    lesson: &Lesson,
    now: &str,
) -> Result<(), AnyError> {
    let start = NaiveDateTime::new(date, parse_time(&lesson.time_start)?);
    let end = NaiveDateTime::new(date, parse_time(&lesson.time_end)?);
    let uid = event_uid(date, lesson);

    let location = [lesson.building.as_deref(), lesson.room.as_deref()]
        .into_iter()
        .flatten()
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join(", ");

    let mut details = Vec::new();
    add_detail(&mut details, "Тип", lesson.lesson_type.as_deref());
    add_detail(
        &mut details,
        "Преподаватель",
        lesson.teacher_name.as_deref(),
    );
    add_detail(&mut details, "Группа", lesson.group.as_deref());
    add_detail(&mut details, "Примечание", lesson.note.as_deref());

    push_line(output, "BEGIN:VEVENT");
    push_line(output, &format!("UID:{uid}"));
    push_line(output, &format!("DTSTAMP:{now}"));
    push_line(output, &format!("LAST-MODIFIED:{now}"));
    push_line(
        output,
        &format!(
            "DTSTART;TZID=Europe/Moscow:{}",
            start.format("%Y%m%dT%H%M%S")
        ),
    );
    push_line(
        output,
        &format!("DTEND;TZID=Europe/Moscow:{}", end.format("%Y%m%dT%H%M%S")),
    );
    push_line(output, &format!("SUMMARY:{}", escape_text(&lesson.subject)));

    if !location.is_empty() {
        push_line(output, &format!("LOCATION:{}", escape_text(&location)));
    }
    if !details.is_empty() {
        push_line(
            output,
            &format!("DESCRIPTION:{}", escape_text(&details.join("\n"))),
        );
    }

    push_line(output, "STATUS:CONFIRMED");
    push_line(output, "TRANSP:OPAQUE");
    push_line(output, "END:VEVENT");
    Ok(())
}

fn event_uid(date: NaiveDate, lesson: &Lesson) -> String {
    if let Some(id) = lesson.id.as_deref() {
        let digest = Sha256::digest(format!("itmo-id:{id}").as_bytes());
        return format!("{digest:x}@itmo-calendar-sync.local");
    }

    let stable_source = format!(
        "{}|{}|{}|{}|{}",
        date,
        lesson.time_start,
        lesson.subject,
        lesson.lesson_type.as_deref().unwrap_or_default(),
        lesson.group.as_deref().unwrap_or_default()
    );
    let digest = Sha256::digest(stable_source.as_bytes());
    format!("{digest:x}@itmo-calendar-sync.local")
}

fn parse_time(value: &str) -> Result<NaiveTime, AnyError> {
    for format in ["%H:%M:%S", "%H:%M"] {
        if let Ok(time) = NaiveTime::parse_from_str(value, format) {
            return Ok(time);
        }
    }

    Err(any_error(format!(
        "ITMO returned an invalid lesson time: {value}"
    )))
}

fn add_detail(target: &mut Vec<String>, label: &str, value: Option<&str>) {
    if let Some(value) = value.filter(|value| !value.is_empty()) {
        target.push(format!("{label}: {value}"));
    }
}

fn escape_text(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace(';', "\\;")
        .replace(',', "\\,")
}

fn push_line(output: &mut String, line: &str) {
    const MAX_BYTES: usize = 73;

    let mut line_bytes = 0;
    for character in line.chars() {
        let width = character.len_utf8();
        if line_bytes > 0 && line_bytes + width > MAX_BYTES {
            output.push_str("\r\n ");
            line_bytes = 1;
        }
        output.push(character);
        line_bytes += width;
    }
    output.push_str("\r\n");
}

#[cfg(test)]
mod tests {
    use super::escape_text;

    #[test]
    fn escapes_ical_text() {
        assert_eq!(escape_text("A, B; C\\D\nE"), "A\\, B\\; C\\\\D\\nE");
    }
}
