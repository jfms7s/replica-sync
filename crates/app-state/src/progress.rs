//! Progress payloads sent to the UI, and the throttle that limits them.

use crate::error::AppError;
use replica_sync_core::session::SessionCounters;
use serde::Serialize;
use std::time::{Duration, Instant};

pub struct Throttle {
    every: Duration,
    last: Option<Instant>,
}

impl Throttle {
    pub fn new(every: Duration) -> Throttle {
        Throttle { every, last: None }
    }

    /// True at most once per interval.
    pub fn ready(&mut self) -> bool {
        let now = Instant::now();
        if self
            .last
            .is_some_and(|l| now.duration_since(l) < self.every)
        {
            return false;
        }
        self.last = Some(now);
        true
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SideProgress {
    pub files: u64,
    pub bytes: u64,
    pub current: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanProgressView {
    pub source: SideProgress,
    pub replica: SideProgress,
    /// Files in the last scan of the source, for an approximate bar.
    pub approx_files: Option<u64>,
}

impl ScanProgressView {
    pub fn read(c: &SessionCounters, approx_files: Option<u64>) -> ScanProgressView {
        let side = |(files, bytes, current): (u64, u64, String)| SideProgress {
            files,
            bytes,
            current,
        };
        ScanProgressView {
            source: side(c.source.read()),
            replica: side(c.replica.read()),
            approx_files,
        }
    }
}

/// The payload of `scan-done` / `apply-done`: exactly one of the two is set.
#[derive(Clone, Debug, Serialize)]
pub struct JobDone<T> {
    pub ok: Option<T>,
    pub error: Option<AppError>,
}

impl<T> From<Result<T, AppError>> for JobDone<T> {
    fn from(r: Result<T, AppError>) -> JobDone<T> {
        match r {
            Ok(v) => JobDone {
                ok: Some(v),
                error: None,
            },
            Err(e) => JobDone {
                ok: None,
                error: Some(e),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn throttle_lets_one_event_through_per_interval() {
        let mut t = Throttle::new(Duration::from_millis(50));
        assert!(t.ready());
        assert!(!t.ready());
        std::thread::sleep(Duration::from_millis(60));
        assert!(t.ready());
    }

    #[test]
    fn job_done_from_result() {
        let ok: JobDone<u8> = Ok(1).into();
        assert_eq!((ok.ok, ok.error), (Some(1), None));
        let err: JobDone<u8> = Err(AppError::new("io")).into();
        assert_eq!(err.error.unwrap().code, "io");
    }
}
