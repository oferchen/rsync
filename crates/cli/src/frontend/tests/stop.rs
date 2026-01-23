use super::common::*;
use super::*;
use std::ffi::OsStr;
use std::time::{Duration, SystemTime};
use time::{Duration as TimeDuration, OffsetDateTime};

#[test]
fn stop_after_argument_requires_positive_minutes() {
    let error = parse_stop_after_argument(OsStr::new("0")).expect_err("reject zero");
    assert!(error.to_string().contains("invalid --stop-after value"));

    let error = parse_stop_after_argument(OsStr::new("abc")).expect_err("reject text");
    assert!(error.to_string().contains("invalid --stop-after value"));
}

#[test]
fn stop_after_argument_returns_future_deadline() {
    let deadline = parse_stop_after_argument(OsStr::new("10")).expect("parse");
    let now = SystemTime::now();
    let delta = deadline
        .duration_since(now)
        .expect("deadline should be in the future");
    assert!(delta >= Duration::from_secs(590));
}

#[test]
fn stop_at_argument_accepts_future_time() {
    let now = OffsetDateTime::now_local().expect("local time");
    // Add 2 minutes but ensure we don't wrap past midnight by checking the hour
    let future = now + TimeDuration::minutes(2);

    // Skip this test if we're near midnight (would wrap to next day)
    if future.hour() < now.hour() {
        // Time wrapped past midnight, skip test
        return;
    }

    let formatted = format!("{:02}:{:02}", future.hour(), future.minute());
    let deadline = parse_stop_at_argument(OsStr::new(&formatted)).expect("parse");

    assert!(deadline > SystemTime::now());
}

#[test]
fn stop_at_argument_rejects_invalid_format() {
    let error = parse_stop_at_argument(OsStr::new("not-a-time")).expect_err("reject");
    assert!(error.to_string().contains("invalid --stop-at format"));
}

#[test]
fn stop_at_argument_rejects_past_time() {
    let now = OffsetDateTime::now_local().expect("local time");
    let past = now - TimeDuration::minutes(1);
    let formatted = format!("{:02}:{:02}", past.hour(), past.minute());
    let error = parse_stop_at_argument(OsStr::new(&formatted)).expect_err("reject past");
    assert!(
        error
            .to_string()
            .contains("--stop-at time is not in the future")
    );
}
