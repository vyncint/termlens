//! The deadline/backoff engine behind every `wait_*`.
//!
//! One [`Monitor`] pairs the shared emulator state with a condvar. The PTY
//! reader thread mutates-and-notifies; waiting calls block on the condvar
//! with a bounded timeout, so time-based conditions (deadlines, quiet
//! periods) fire even when no bytes ever arrive.

use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// Upper bound on any single condvar sleep inside a wait loop. Wakes are
/// normally notification-driven; the cap guarantees deadline checks can
/// never be postponed indefinitely by a silent child.
pub(crate) const POLL_CAP: Duration = Duration::from_millis(50);

/// First step of the poll ladder used where no condvar applies
/// (e.g. `try_wait`-polling the child process).
pub(crate) const INITIAL_BACKOFF: Duration = Duration::from_millis(1);

/// Exponential backoff, capped so a late-exiting child is still observed
/// within ~20ms of exiting.
pub(crate) fn next_backoff(current: Duration) -> Duration {
    (current * 2).min(Duration::from_millis(20))
}

/// Marker error: the deadline expired inside [`Monitor::wait_until`].
#[derive(Debug)]
pub(crate) struct Expired;

/// State + condvar, with poison recovery.
pub(crate) struct Monitor<S> {
    state: Mutex<S>,
    cond: Condvar,
}

impl<S> Monitor<S> {
    pub(crate) fn new(state: S) -> Self {
        Self {
            state: Mutex::new(state),
            cond: Condvar::new(),
        }
    }

    /// Lock the state. Poisoning is swallowed deliberately: if the reader
    /// thread died panicking, the state it left behind is still the best
    /// available evidence for error reports.
    pub(crate) fn lock(&self) -> MutexGuard<'_, S> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Mutate under the lock, then wake all waiters.
    pub(crate) fn mutate(&self, f: impl FnOnce(&mut S)) {
        {
            let mut guard = self.lock();
            f(&mut guard);
        }
        self.cond.notify_all();
    }

    /// One bounded condvar sleep; returns the reacquired guard.
    pub(crate) fn wait_timeout<'a>(
        &self,
        guard: MutexGuard<'a, S>,
        dur: Duration,
    ) -> MutexGuard<'a, S> {
        self.cond
            .wait_timeout(guard, dur)
            .unwrap_or_else(PoisonError::into_inner)
            .0
    }

    /// Block until `check` yields `Some`, or `deadline` passes.
    ///
    /// `check` runs under the lock on every wake (notification or poll
    /// tick), so it must stay cheap.
    pub(crate) fn wait_until<T>(
        &self,
        deadline: Instant,
        mut check: impl FnMut(&mut S) -> Option<T>,
    ) -> Result<T, Expired> {
        let mut guard = self.lock();
        loop {
            if let Some(v) = check(&mut guard) {
                return Ok(v);
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(Expired);
            }
            let sleep = (deadline - now).min(POLL_CAP);
            guard = self.wait_timeout(guard, sleep);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn wait_until_wakes_on_notification() {
        let monitor = Arc::new(Monitor::new(0u32));
        let m2 = Arc::clone(&monitor);
        let handle = thread::spawn(move || {
            m2.mutate(|v| *v = 7);
        });
        let got = monitor
            .wait_until(Instant::now() + Duration::from_secs(5), |v| {
                (*v == 7).then_some(*v)
            })
            .ok();
        handle.join().unwrap();
        assert_eq!(got, Some(7));
    }

    #[test]
    fn wait_until_expires_at_the_deadline() {
        let monitor = Monitor::new(());
        let start = Instant::now();
        let res = monitor.wait_until(start + Duration::from_millis(60), |()| None::<()>);
        assert!(res.is_err());
        assert!(start.elapsed() >= Duration::from_millis(60));
    }

    #[test]
    fn backoff_doubles_and_caps() {
        let mut d = INITIAL_BACKOFF;
        let mut seen = Vec::new();
        for _ in 0..8 {
            seen.push(d.as_millis());
            d = next_backoff(d);
        }
        assert_eq!(seen, vec![1, 2, 4, 8, 16, 20, 20, 20]);
    }

    #[test]
    fn lock_recovers_from_poison() {
        let monitor = Arc::new(Monitor::new(41u32));
        let m2 = Arc::clone(&monitor);
        let _ = thread::spawn(move || {
            let _guard = m2.lock();
            panic!("poison the mutex");
        })
        .join();
        let mut guard = monitor.lock();
        *guard += 1;
        assert_eq!(*guard, 42);
    }
}
