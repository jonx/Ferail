// SPDX-License-Identifier: MIT
// Copyright (c) 2026 John Knipper

//! One session-owned writer. Closing admission releases the last sender before
//! joining, so a blocked receiver can terminate without UI or executor work.
use std::io;
use std::sync::{Mutex, mpsc};
use std::thread::{self, JoinHandle, ThreadId};

struct State {
    closed: bool,
    sender: Option<mpsc::Sender<String>>,
    worker: Option<JoinHandle<()>>,
}

pub(crate) struct StateWriter {
    owner: ThreadId,
    state: Mutex<State>,
}

impl StateWriter {
    pub(crate) fn new() -> Self {
        Self {
            owner: thread::current().id(),
            state: Mutex::new(State {
                closed: false,
                sender: None,
                worker: None,
            }),
        }
    }

    pub(crate) fn send(&self, contents: String, write: fn(&str)) -> io::Result<()> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.closed {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "state writer admission closed",
            ));
        }
        if state.sender.is_none() {
            let (sender, receiver) = mpsc::channel::<String>();
            let worker = thread::Builder::new()
                .name("app-state-writer".into())
                .spawn(move || {
                    while let Ok(mut latest) = receiver.recv() {
                        while let Ok(newer) = receiver.try_recv() {
                            latest = newer;
                        }
                        write(&latest);
                    }
                })?;
            state.sender = Some(sender);
            state.worker = Some(worker);
        }
        state
            .sender
            .as_ref()
            .expect("initialized writer sender")
            .send(contents)
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "state writer stopped"))
    }

    /// Only the session owner may wait. The worker runs a plain disk writer,
    /// never a foreground callback, so joining cannot require the UI loop.
    pub(crate) fn shutdown(&self) -> io::Result<()> {
        if thread::current().id() != self.owner {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "state writer shutdown requires session owner",
            ));
        }
        let worker = {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            state.closed = true;
            drop(state.sender.take());
            state.worker.take()
        };
        if let Some(worker) = worker {
            worker
                .join()
                .map_err(|_| io::Error::other("state writer panicked"))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, OnceLock};
    static WRITTEN: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
    fn record(value: &str) {
        WRITTEN
            .get_or_init(|| Mutex::new(Vec::new()))
            .lock()
            .unwrap()
            .push(value.into());
    }
    #[test]
    fn closes_receiver_drains_latest_and_joins() {
        let writer = StateWriter::new();
        writer.send("first".into(), record).unwrap();
        writer.send("last".into(), record).unwrap();
        writer.shutdown().unwrap();
        assert_eq!(
            WRITTEN.get().unwrap().lock().unwrap().last().unwrap(),
            "last"
        );
        assert!(writer.state.lock().unwrap().worker.is_none());
        assert_eq!(
            writer.send("late".into(), record).unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
        writer.shutdown().unwrap();
    }
    #[test]
    fn wrong_owner_cannot_take_or_detach_worker() {
        let writer = Arc::new(StateWriter::new());
        let other = writer.clone();
        assert_eq!(
            thread::spawn(move || other.shutdown().unwrap_err().kind())
                .join()
                .unwrap(),
            io::ErrorKind::PermissionDenied
        );
        writer.shutdown().unwrap();
    }
    #[test]
    fn shutdown_without_start_does_not_spawn() {
        let writer = StateWriter::new();
        writer.shutdown().unwrap();
        assert!(writer.state.lock().unwrap().worker.is_none());
    }
    #[test]
    fn panic_is_joined_and_reported() {
        fn fail(_: &str) {
            panic!("writer fixture");
        }
        let writer = StateWriter::new();
        writer.send("value".into(), fail).unwrap();
        assert!(writer.shutdown().is_err());
        assert!(writer.state.lock().unwrap().worker.is_none());
    }
}
