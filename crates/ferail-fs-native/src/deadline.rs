//! Bounded calls into filesystems that may never answer.
//!
//! A mounted volume whose server vanished, whose disk was pulled mid-read, or
//! whose FUSE process stalled makes every `stat` under it block, often for
//! good. No timeout exists on the syscall itself, so the only defence is to
//! make the call on a thread of its own and stop waiting for it.
//!
//! [`start`] runs a probe on a dedicated thread; [`Probe::wait_until`] waits
//! for it up to a deadline and reports `None` when the deadline passes first.
//! The probe thread is left behind, blocked in the kernel: nothing can cancel
//! it. To keep a volume that never answers from collecting one stuck thread
//! per refresh, a probe whose key is still running from an earlier call is
//! not started again; its [`Probe`] reports `None` at once.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, sync_channel};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

/// Identity of one kind of probe on one path. The purpose keeps two
/// unrelated probes of the same healthy volume from being mistaken for a
/// repeat of each other.
pub type ProbeKey = (&'static str, PathBuf);

static IN_FLIGHT: LazyLock<Mutex<HashSet<ProbeKey>>> = LazyLock::new(Default::default);

/// A probe started by [`start`].
pub struct Probe<T> {
    rx: Option<Receiver<T>>,
}

impl<T> Probe<T> {
    /// The probe's answer, or `None` when it did not arrive before
    /// `deadline` (or was never started because an earlier one with the same
    /// key is still blocked).
    pub fn wait_until(self, deadline: Instant) -> Option<T> {
        let rx = self.rx?;
        let timeout = deadline.saturating_duration_since(Instant::now());
        rx.recv_timeout(timeout).ok()
    }
}

/// Run `probe` on its own thread. See the module docs.
pub fn start<T, F>(key: ProbeKey, probe: F) -> Probe<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    if !IN_FLIGHT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(key.clone())
    {
        return Probe { rx: None };
    }
    let (tx, rx) = sync_channel(1);
    let spawned = std::thread::Builder::new()
        .name("fs-deadline-probe".into())
        .spawn({
            let key = key.clone();
            move || {
                let value = probe();
                IN_FLIGHT
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .remove(&key);
                let _ = tx.send(value);
            }
        });
    if spawned.is_err() {
        IN_FLIGHT
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&key);
        return Probe { rx: None };
    }
    Probe { rx: Some(rx) }
}

/// [`start`] then [`Probe::wait_until`] `timeout` from now.
pub fn run<T, F>(key: ProbeKey, timeout: Duration, probe: F) -> Option<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    start(key, probe).wait_until(Instant::now() + timeout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    #[test]
    fn a_prompt_probe_answers() {
        let got = run(("test-prompt", PathBuf::from("/a")), Duration::from_secs(5), || 7);
        assert_eq!(got, Some(7));
    }

    #[test]
    fn a_blocked_probe_times_out_and_is_not_started_twice() {
        let key: ProbeKey = ("test-blocked", PathBuf::from("/stuck"));
        let (release_tx, release_rx) = channel::<()>();
        let started = Instant::now();
        let first = run(key.clone(), Duration::from_millis(50), move || {
            let _ = release_rx.recv();
            1
        });
        assert_eq!(first, None);
        assert!(started.elapsed() < Duration::from_secs(2));

        // Still blocked: the repeat returns at once without a new thread.
        let repeat_started = Instant::now();
        let second = run(key.clone(), Duration::from_secs(5), || 2);
        assert_eq!(second, None);
        assert!(repeat_started.elapsed() < Duration::from_millis(500));

        // Once the stuck probe finally returns, the key is free again.
        release_tx.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(v) = run(key.clone(), Duration::from_secs(5), || 3) {
                assert_eq!(v, 3);
                break;
            }
            assert!(Instant::now() < deadline, "key never released");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn one_stuck_probe_does_not_delay_the_others() {
        let (_hold, stuck_rx) = channel::<()>();
        let deadline = Instant::now() + Duration::from_millis(200);
        let stuck = start(("test-many", PathBuf::from("/dead")), move || {
            let _ = stuck_rx.recv();
        });
        let healthy: Vec<_> = (0..4)
            .map(|i| start(("test-many", PathBuf::from(format!("/ok{i}"))), move || i))
            .collect();
        assert!(stuck.wait_until(deadline).is_none());
        let answers: Vec<_> = healthy.into_iter().map(|p| p.wait_until(deadline)).collect();
        assert_eq!(answers, vec![Some(0), Some(1), Some(2), Some(3)]);
    }
}
