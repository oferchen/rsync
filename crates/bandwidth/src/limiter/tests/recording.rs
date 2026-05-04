use super::{
    BandwidthLimiter, MINIMUM_SLEEP_MICROS, RecordedSleepSession, recorded_sleep_session,
    recorded_sleeps, sleep_for,
};
use std::num::NonZeroU64;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Duration;

#[test]
fn recorded_sleep_session_into_vec_consumes_guard() {
    let mut session = recorded_sleep_session();
    session.clear();

    let mut limiter = BandwidthLimiter::new(NonZeroU64::new(1024).unwrap());
    let _ = limiter.register(2048);

    let recorded = session.into_vec();
    assert!(!recorded.is_empty());

    let mut follow_up = recorded_sleep_session();
    assert!(follow_up.is_empty());
    let _ = follow_up.take();
}

#[test]
fn recorded_sleep_session_into_iter_collects_durations() {
    let mut session = recorded_sleep_session();
    session.clear();

    sleep_for(Duration::from_micros(MINIMUM_SLEEP_MICROS as u64));

    let durations: Vec<_> = session.into_iter().collect();
    assert_eq!(
        durations,
        [Duration::from_micros(MINIMUM_SLEEP_MICROS as u64)]
    );

    let mut follow_up = recorded_sleep_session();
    assert!(follow_up.is_empty());
    let _ = follow_up.take();
}

#[test]
fn recorded_sleep_session_iter_preserves_buffer_and_reports_length() {
    let mut session = recorded_sleep_session();
    session.clear();

    let expected = vec![
        Duration::from_micros(MINIMUM_SLEEP_MICROS as u64),
        Duration::from_micros((MINIMUM_SLEEP_MICROS / 2) as u64),
    ];

    for duration in &expected {
        sleep_for(*duration);
    }

    let mut iterator = session.iter();
    assert_eq!(iterator.len(), expected.len());
    assert_eq!(iterator.size_hint(), (expected.len(), Some(expected.len())));
    assert_eq!(iterator.next(), Some(expected[0]));
    assert_eq!(iterator.len(), expected.len() - 1);
    assert_eq!(iterator.next(), Some(expected[1]));
    assert!(iterator.next().is_none());
    assert_eq!(iterator.len(), 0);
    drop(iterator);

    assert_eq!(session.len(), expected.len());
    assert_eq!(session.iter().collect::<Vec<_>>(), expected);
    assert_eq!(session.take(), expected);
}

#[test]
fn recorded_sleep_session_reference_into_iter_preserves_buffer() {
    let mut session = recorded_sleep_session();
    session.clear();

    let expected = vec![
        Duration::from_micros(MINIMUM_SLEEP_MICROS as u64),
        Duration::from_micros((MINIMUM_SLEEP_MICROS / 2) as u64),
    ];

    for duration in &expected {
        sleep_for(*duration);
    }

    let collected: Vec<_> = (&session).into_iter().collect();
    assert_eq!(collected, expected);
    assert_eq!(session.len(), expected.len());
    assert_eq!(session.take(), expected);
}

#[test]
fn recorded_sleep_session_mut_reference_into_iter_preserves_buffer() {
    let mut session = recorded_sleep_session();
    session.clear();

    let expected = vec![
        Duration::from_micros(MINIMUM_SLEEP_MICROS as u64),
        Duration::from_micros((MINIMUM_SLEEP_MICROS / 2) as u64),
    ];

    for duration in &expected {
        sleep_for(*duration);
    }

    let mut observed = Vec::new();
    for chunk in &mut session {
        observed.push(chunk);
    }

    assert_eq!(observed, expected);
    assert_eq!(session.len(), expected.len());
    assert_eq!(session.take(), expected);
}

#[test]
fn recorded_sleep_session_snapshot_preserves_buffer() {
    let mut session = recorded_sleep_session();
    session.clear();

    sleep_for(Duration::from_micros(MINIMUM_SLEEP_MICROS as u64));

    let snapshot = session.snapshot();
    assert_eq!(snapshot, session.snapshot());
    assert_eq!(session.len(), snapshot.len());
    assert_eq!(session.take(), snapshot);
}

#[test]
fn recorded_sleep_session_last_duration_observes_latest_sample() {
    let mut session = recorded_sleep_session();
    session.clear();

    assert!(session.last_duration().is_none());

    let first = Duration::from_micros(MINIMUM_SLEEP_MICROS as u64);
    sleep_for(first);
    assert_eq!(session.last_duration(), Some(first));

    let second = Duration::from_micros((MINIMUM_SLEEP_MICROS / 2) as u64);
    sleep_for(second);
    assert_eq!(session.last_duration(), Some(second));
    assert_eq!(session.len(), 2);
}

#[test]
fn recorded_sleep_session_total_duration_reports_sum_without_draining() {
    let mut session = recorded_sleep_session();
    session.clear();

    sleep_for(Duration::from_micros(MINIMUM_SLEEP_MICROS as u64));
    sleep_for(Duration::from_micros(MINIMUM_SLEEP_MICROS as u64 / 2));

    let total = session.total_duration();
    assert_eq!(
        total,
        Duration::from_micros((MINIMUM_SLEEP_MICROS + MINIMUM_SLEEP_MICROS / 2) as u64)
    );
    assert_eq!(session.len(), 2);
    assert_eq!(session.total_duration(), total);
    assert_eq!(
        session.take(),
        [
            Duration::from_micros(MINIMUM_SLEEP_MICROS as u64),
            Duration::from_micros((MINIMUM_SLEEP_MICROS / 2) as u64),
        ]
    );
}

#[test]
fn recorded_sleep_iter_supports_double_ended_iteration() {
    let mut session = recorded_sleep_session();
    session.clear();

    let first = Duration::from_micros(MINIMUM_SLEEP_MICROS as u64);
    let second = Duration::from_micros((MINIMUM_SLEEP_MICROS / 2) as u64);
    sleep_for(first);
    sleep_for(second);

    {
        let mut iter = session.iter();
        assert_eq!(iter.next(), Some(first));
        assert_eq!(iter.next_back(), Some(second));
        assert_eq!(iter.next(), None);
        assert_eq!(iter.next_back(), None);
    }

    let collected: Vec<_> = session.iter().rev().collect();
    assert_eq!(collected, [second, first]);
    assert_eq!(session.len(), 2);
}

#[test]
fn recorded_sleep_session_default_acquires_guard() {
    let mut session = RecordedSleepSession::default();
    session.clear();

    sleep_for(Duration::from_micros(MINIMUM_SLEEP_MICROS as u64));

    assert_eq!(
        session.take(),
        [Duration::from_micros(MINIMUM_SLEEP_MICROS as u64)]
    );
}

#[test]
fn recorded_sleep_session_recovers_from_mutex_poisoning() {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let _session = recorded_sleep_session();
        panic!("poison session mutex");
    }));

    let _ = catch_unwind(AssertUnwindSafe(|| {
        let _guard = recorded_sleeps()
            .lock()
            .expect("lock recorded sleeps to poison");
        panic!("poison recorded sleeps mutex");
    }));

    let mut session = recorded_sleep_session();
    session.clear();

    sleep_for(Duration::from_micros(MINIMUM_SLEEP_MICROS as u64));

    let recorded = session.take();
    assert_eq!(
        recorded,
        [Duration::from_micros(MINIMUM_SLEEP_MICROS as u64)]
    );
}
