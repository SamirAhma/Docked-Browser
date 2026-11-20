use serde_json::{Map, Value, json};
use std::fs;
use std::io;

use super::{
    MYT_OFFSET_SECS, days_from_civil, iso_weekday, now_f64, paths, profile_ok, read_json, week_key,
};

fn parse_week(key: &str) -> Option<i64> {
    let (year, week) = key.split_once("-W")?;
    let (year, week): (i64, i64) = (year.parse().ok()?, week.parse().ok()?);
    if !(1..=9999).contains(&year) || !(1..=53).contains(&week) {
        return None;
    }
    let jan4 = days_from_civil(year, 1, 4);
    let monday = jan4 - iso_weekday(jan4) + 1 + (week - 1) * 7;
    (week_key(monday) == key).then_some(monday)
}

fn parse_day(key: &str) -> Option<i64> {
    let mut parts = key.split('-');
    let (year, month, day): (i64, i64, i64) = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    if parts.next().is_some()
        || !(1..=9999).contains(&year)
        || !(1..=12).contains(&month)
        || day < 1
    {
        return None;
    }
    let start = days_from_civil(year, month, 1);
    let end = if month == 12 {
        days_from_civil(year + 1, 1, 1)
    } else {
        days_from_civil(year, month + 1, 1)
    };
    (day <= end - start).then_some(start + day - 1)
}

fn record(weeks: &mut Map<String, Value>, profile: &str, day: i64, hour: i64) -> bool {
    let week = weeks.entry(week_key(day)).or_insert_with(|| json!({}));
    if !week.is_object() {
        *week = json!({});
    }
    let slot = format!("{}-{hour:02}", iso_weekday(day));
    let names = week
        .as_object_mut()
        .unwrap()
        .entry(slot)
        .or_insert_with(|| json!([]));
    if !names.is_array() {
        *names = json!([]);
    }
    let names = names.as_array_mut().unwrap();
    if names.iter().any(|name| name.as_str() == Some(profile)) {
        return false;
    }
    names.push(json!(profile));
    names.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
    true
}

fn update<T>(
    edit: impl FnOnce(&mut Map<String, Value>, &mut Map<String, Value>, i64, i64) -> T,
) -> io::Result<T> {
    let dir = &paths().state_dir;
    fs::create_dir_all(dir)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(dir.join("habit.json.lock"))?;
    lock.lock()?;
    let path = dir.join("habit.json");
    let data = read_json(&path).unwrap_or(Value::Null);
    let object = |key: &str| {
        data.get(key)
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default()
    };
    let mut weeks = object("weeks");
    let mut suggested = object("suggested");
    let myt = now_f64() as i64 + MYT_OFFSET_SECS;
    let day = myt.div_euclid(86400);
    let hour = myt.rem_euclid(86400) / 3600;
    let result = edit(&mut weeks, &mut suggested, day, hour);
    let monday = day - iso_weekday(day) + 1;
    weeks.retain(|key, _| parse_week(key).is_some_and(|start| start >= monday - 49));
    suggested.retain(|key, _| parse_day(key).is_some_and(|date| date >= day - 56));
    if weeks != object("weeks") || suggested != object("suggested") {
        let temp = dir.join("habit.json.tmp");
        let saved = json!({"weeks": weeks, "suggested": suggested});
        fs::write(&temp, serde_json::to_string_pretty(&saved)? + "\n")?;
        fs::rename(temp, path)?;
    }
    Ok(result)
}

pub(super) fn log_use(profile: &str) -> io::Result<bool> {
    if !profile_ok(profile) {
        return Ok(false);
    }
    update(|weeks, _, day, hour| record(weeks, profile, day, hour))
}

fn date_key(day: i64) -> String {
    let time = day * 86400;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::gmtime_r(&time, &mut tm) };
    format!(
        "{:04}-{:02}-{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday
    )
}

pub(super) fn offer(focused: &str) -> io::Result<Option<(String, String, String)>> {
    update(|weeks, suggested, day, hour| {
        let week_data = json!({"weeks": weeks});
        let profile = super::slot_winner(&week_data, day, hour)?;
        if profile.eq_ignore_ascii_case(focused) {
            return None;
        }
        let date = date_key(day);
        let slot = format!("{}-{hour:02}", iso_weekday(day));
        let slots = suggested.entry(date.clone()).or_insert_with(|| json!([]));
        if !slots.is_array() {
            *slots = json!([]);
        }
        let slots = slots.as_array_mut()?;
        if slots.iter().any(|key| key == &slot) {
            return None;
        }
        slots.push(json!(slot));
        slots.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
        Some((profile, date, slot))
    })
}

pub(super) fn clear_offer(date: &str, slot: &str) -> io::Result<()> {
    update(|_, suggested, _, _| {
        if let Some(slots) = suggested.get_mut(date).and_then(Value::as_array_mut) {
            slots.retain(|key| key != slot);
            if slots.is_empty() {
                suggested.remove(date);
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_exact_profile_in_malaysia_iso_week() {
        let day = days_from_civil(2021, 1, 1);
        let mut weeks = Map::new();
        assert!(record(&mut weeks, "Work", day, 9));
        assert!(!record(&mut weeks, "Work", day, 9));
        assert!(record(&mut weeks, "work", day, 9));
        assert_eq!(weeks["2020-W53"]["5-09"], json!(["Work", "work"]));
        assert_eq!(parse_week("2020-W53"), Some(days_from_civil(2020, 12, 28)));
        assert_eq!(parse_week("2021-W53"), None);
        assert_eq!(parse_day("2021-02-29"), None);
    }
}
