// SPDX-License-Identifier: MIT
// Copyright (c) 2026 John Knipper
//! Ferail file references in FORM FTXT: CHRS text plus versioned FSAF metadata.
//! CHRS is display interoperability only, never parsed as a file list.
use super::ClipboardFileOperation;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
const MAX: usize = 2 * 1024 * 1024;
const MAX_ITEMS: usize = 20_000;
const MAX_PATH: usize = 32_768;
fn invalid(what: &str) -> String {
    format!("Invalid file clipboard: {what}")
}
fn path_valid(bytes: &[u8]) -> bool {
    let Some(colon) = bytes.iter().position(|b| *b == b':') else {
        return false;
    };
    !bytes.is_empty()
        && bytes.len() <= MAX_PATH
        && bytes[0] != b'/'
        && colon > 0
        && !bytes[..colon].contains(&b'/')
        && !bytes.contains(&0)
        && !bytes[colon + 1..].contains(&b':')
}
fn u32_at(bytes: &[u8], at: usize) -> Result<usize, String> {
    let raw = bytes
        .get(at..at.checked_add(4).ok_or_else(|| invalid("overflow"))?)
        .ok_or_else(|| invalid("truncated length"))?;
    Ok(u32::from_be_bytes(raw.try_into().unwrap()) as usize)
}
fn chunk(out: &mut Vec<u8>, id: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(id);
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(data);
    if data.len() & 1 != 0 {
        out.push(0);
    }
}
fn encode(items: &[(&Path, bool)], op: ClipboardFileOperation) -> Result<Vec<u8>, String> {
    if items.is_empty() || items.len() > MAX_ITEMS {
        return Err(invalid("file count limit"));
    }
    let mut payload = vec![
        0,
        1,
        if op == ClipboardFileOperation::Move {
            1
        } else {
            0
        },
        0,
    ];
    payload.extend_from_slice(&(items.len() as u32).to_be_bytes());
    let mut text = Vec::new();
    for (path, directory) in items {
        let bytes = path.as_os_str().as_bytes();
        if !path_valid(bytes) {
            return Err(invalid("absolute native DOS path required"));
        }
        if payload.len() + text.len() + 2 * bytes.len() + 64 > MAX {
            return Err(invalid("size limit"));
        }
        payload.extend_from_slice(&[u8::from(*directory), 0, 0, 0]);
        payload.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
        payload.extend_from_slice(bytes);
        text.extend_from_slice(bytes);
        text.push(b'\n');
    }
    let mut body = b"FTXT".to_vec();
    chunk(&mut body, b"CHRS", &text);
    chunk(&mut body, b"FSAF", &payload);
    if body.len() + 8 > MAX {
        return Err(invalid("size limit"));
    }
    let mut out = b"FORM".to_vec();
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend(body);
    Ok(out)
}
fn payload(bytes: &[u8]) -> Result<(Vec<PathBuf>, ClipboardFileOperation), String> {
    if bytes.len() < 8 || bytes[..2] != [0, 1] || bytes[3] != 0 {
        return Err(invalid("version or header"));
    }
    let op = match bytes[2] {
        0 => ClipboardFileOperation::Copy,
        1 => ClipboardFileOperation::Move,
        _ => return Err(invalid("operation")),
    };
    let count = u32_at(bytes, 4)?;
    if count == 0 || count > MAX_ITEMS || count > (bytes.len() - 8) / 9 {
        return Err(invalid("file count"));
    }
    let mut pos = 8;
    let mut paths = Vec::with_capacity(count);
    for _ in 0..count {
        let flags = bytes
            .get(pos..pos + 4)
            .ok_or_else(|| invalid("truncated entry"))?;
        if flags[0] > 1 || flags[1..] != [0, 0, 0] {
            return Err(invalid("entry flags"));
        }
        let length = u32_at(bytes, pos + 4)?;
        pos += 8;
        let end = pos
            .checked_add(length)
            .ok_or_else(|| invalid("entry overflow"))?;
        let name = bytes
            .get(pos..end)
            .ok_or_else(|| invalid("truncated path"))?;
        if !path_valid(name) {
            return Err(invalid("path"));
        }
        paths.push(PathBuf::from(OsString::from_vec(name.to_vec())));
        pos = end;
    }
    if pos != bytes.len() {
        return Err(invalid("trailing entry data"));
    }
    Ok((paths, op))
}
fn decode(bytes: &[u8]) -> Result<Option<(Vec<PathBuf>, ClipboardFileOperation)>, String> {
    if bytes.len() > MAX {
        return Err(invalid("size limit"));
    }
    if !bytes.starts_with(b"FORM") {
        return Ok(None);
    }
    if bytes.len() < 12 || u32_at(bytes, 4)?.checked_add(8) != Some(bytes.len()) {
        return Err(invalid("FORM size"));
    }
    if &bytes[8..12] != b"FTXT" {
        return Ok(None);
    }
    let mut pos = 12;
    let mut result = None;
    let mut chunks = 0;
    while pos < bytes.len() {
        chunks += 1;
        if chunks > 128 {
            return Err(invalid("chunk count"));
        }
        let id = bytes
            .get(pos..pos + 4)
            .ok_or_else(|| invalid("truncated chunk"))?;
        let length = u32_at(bytes, pos + 4)?;
        pos += 8;
        let end = pos
            .checked_add(length)
            .ok_or_else(|| invalid("chunk overflow"))?;
        let data = bytes
            .get(pos..end)
            .ok_or_else(|| invalid("truncated chunk data"))?;
        if id == b"FSAF" {
            if result.is_some() {
                return Err(invalid("duplicate FSAF"));
            }
            result = Some(payload(data)?);
        }
        pos = end
            .checked_add(length & 1)
            .ok_or_else(|| invalid("padding overflow"))?;
        if pos > bytes.len() {
            return Err(invalid("missing chunk padding"));
        }
    }
    Ok(result)
}

// UI reservations establish ordering. Only device execution owns the worker;
// its JoinHandle is retained until explicit shutdown before module unload.
#[derive(Default)]
struct Queue {
    next: u64,
    serving: u64,
    canceled: BTreeSet<u64>,
    pending: BTreeMap<u64, Box<dyn FnOnce(bool) + Send>>,
    closed: bool,
    joined: bool,
    joining: bool,
    active: usize,
    worker: Option<std::thread::JoinHandle<()>>,
    write_error: Option<String>,
}
struct Gate {
    state: Mutex<Queue>,
    changed: Condvar,
}
fn new_gate() -> Arc<Gate> {
    Arc::new(Gate {
        state: Mutex::new(Queue::default()),
        changed: Condvar::new(),
    })
}
fn gate() -> &'static Arc<Gate> {
    static G: OnceLock<Arc<Gate>> = OnceLock::new();
    G.get_or_init(new_gate)
}
fn finish(g: &Gate, id: u64, active: bool) {
    let mut s = g.state.lock().unwrap_or_else(|e| e.into_inner());
    if active {
        s.active -= 1;
    }
    s.canceled.insert(id);
    while {
        let at = s.serving;
        s.canceled.remove(&at)
    } {
        s.serving += 1;
    }
    g.changed.notify_all();
}
pub struct Request {
    id: Option<u64>,
    gate: Arc<Gate>,
}
fn reserve_for(g: &Arc<Gate>) -> Result<Request, String> {
    let mut s = g.state.lock().unwrap_or_else(|e| e.into_inner());
    if s.closed {
        return Err("File clipboard is shutting down".into());
    }
    if s.next == u64::MAX || s.next - s.serving >= 64 {
        return Err("File clipboard queue is busy".into());
    }
    let id = s.next;
    s.next += 1;
    Ok(Request {
        id: Some(id),
        gate: g.clone(),
    })
}
pub fn reserve() -> Result<Request, String> {
    reserve_for(gate())
}
impl Drop for Request {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            finish(&self.gate, id, false);
        }
    }
}
pub fn accept_result(sequence: u64) -> bool {
    use std::sync::atomic::{AtomicU64, Ordering};
    static LAST: AtomicU64 = AtomicU64::new(0);
    sequence >= LAST.fetch_max(sequence, Ordering::SeqCst)
}
struct Reply<T> {
    value: Option<T>,
    waker: Option<std::task::Waker>,
}
pub struct Pending<T>(Arc<Mutex<Reply<T>>>);
impl<T> std::future::Future for Pending<T> {
    type Output = T;
    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<T> {
        let mut r = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(v) = r.value.take() {
            std::task::Poll::Ready(v)
        } else {
            r.waker = Some(cx.waker().clone());
            std::task::Poll::Pending
        }
    }
}
fn start_worker(g: &Arc<Gate>) -> Result<(), String> {
    let mut s = g.state.lock().unwrap_or_else(|e| e.into_inner());
    if s.closed {
        return Err("File clipboard is shutting down".into());
    }
    if s.worker.is_some() {
        return Ok(());
    }
    let worker_gate = g.clone();
    s.worker = Some(
        std::thread::Builder::new()
            .name("file-clipboard".into())
            .spawn(move || {
                loop {
                    let mut state = worker_gate.state.lock().unwrap_or_else(|e| e.into_inner());
                    let action = loop {
                        if state.closed {
                            return;
                        }
                        let id = state.serving;
                        if let Some(action) = state.pending.remove(&id) {
                            break action;
                        }
                        state = worker_gate
                            .changed
                            .wait(state)
                            .unwrap_or_else(|e| e.into_inner());
                    };
                    drop(state);
                    action(false);
                }
            })
            .map_err(|e| format!("Cannot start clipboard worker: {e}"))?,
    );
    Ok(())
}
fn shutdown_for(g: &Arc<Gate>) -> Result<(), String> {
    let (queued, worker) = {
        let mut s = g.state.lock().unwrap_or_else(|e| e.into_inner());
        while s.joining {
            s = g.changed.wait(s).unwrap_or_else(|e| e.into_inner());
        }
        if s.joined {
            return Ok(());
        }
        s.closed = true;
        s.joining = true;
        let queued = std::mem::take(&mut s.pending);
        let worker = s.worker.take();
        g.changed.notify_all();
        (queued, worker)
    };
    // Resolve pending futures in error, without running their device actions.
    // Drop/finish occurs outside the queue mutex to avoid a recursive lock.
    for (_, action) in queued {
        action(true);
    }
    let result = worker
        .map(|h| h.join())
        .transpose()
        .map(|_| ())
        .map_err(|_| "Clipboard worker panicked during shutdown".to_string());
    let mut s = g.state.lock().unwrap_or_else(|e| e.into_inner());
    while s.active != 0 {
        s = g.changed.wait(s).unwrap_or_else(|e| e.into_inner());
    }
    s.joined = true;
    s.joining = false;
    g.changed.notify_all();
    result
}
/// Close admission, cancel undispatched requests, then join actual device I/O.
/// Call after the event loop returns, before AROS unloads this command module.
/// A foreign pending CBD_POST or DOS I/O cannot be aborted by clipboard.device;
/// this function deliberately waits rather than detaching a live native task.
pub fn shutdown() -> Result<(), String> {
    shutdown_for(gate())
}
/// Main-task scope guard, declared before dispatch so all normal returns join.
pub struct Session;
impl Default for Session {
    fn default() -> Self {
        Self
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        if let Err(e) = shutdown() {
            eprintln!("{e}");
        }
    }
}
// A failed write cannot turn a queued Paste into a destructive Move of the
// previous clipboard contents. This is error state, never a replacement payload.
fn write_checked(g: &Gate, action: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
    let result = action();
    g.state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .write_error = result.as_ref().err().cloned();
    result
}
fn read_checked(g: &Gate) -> Result<(Vec<PathBuf>, ClipboardFileOperation), String> {
    if let Some(error) = g
        .state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .write_error
        .clone()
    {
        return Err(format!(
            "Previous file clipboard write failed; copy files again before pasting: {error}"
        ));
    }
    Ok(decode(&read_device()?)?.unwrap_or((Vec::new(), ClipboardFileOperation::Copy)))
}
impl Request {
    fn submit<T: Send + 'static>(
        self,
        action: impl FnOnce() -> Result<T, String> + Send + 'static,
    ) -> Result<Pending<Result<T, String>>, String> {
        start_worker(&self.gate)?;
        let reply = Arc::new(Mutex::new(Reply {
            value: None,
            waker: None,
        }));
        let output = reply.clone();
        let g = self.gate.clone();
        let mut state = g.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed {
            return Err("File clipboard is shutting down".into());
        }
        state.pending.insert(
            self.id.unwrap(),
            Box::new(move |cancel| {
                let value = if cancel {
                    drop(self);
                    Err("File clipboard request canceled at shutdown".into())
                } else {
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.run(action)))
                        .unwrap_or_else(|_| Err("File clipboard request panicked".into()))
                };
                let waker = {
                    let mut r = output.lock().unwrap_or_else(|e| e.into_inner());
                    r.value = Some(value);
                    r.waker.take()
                };
                if let Some(w) = waker {
                    w.wake();
                }
            }),
        );
        g.changed.notify_all();
        Ok(Pending(reply))
    }
    pub fn begin_write(
        self,
        items: Vec<(PathBuf, bool)>,
        operation: ClipboardFileOperation,
    ) -> Result<Pending<Result<(), String>>, String> {
        let g = self.gate.clone();
        self.submit(move || {
            write_checked(&g, || {
                let refs: Vec<_> = items.iter().map(|(p, d)| (p.as_path(), *d)).collect();
                write_device(&encode(&refs, operation)?)
            })
        })
    }
    pub fn begin_read(
        self,
    ) -> Result<Pending<Result<(Vec<PathBuf>, ClipboardFileOperation), String>>, String> {
        let g = self.gate.clone();
        self.submit(move || read_checked(&g))
    }
    pub fn sequence(&self) -> u64 {
        self.id.unwrap() + 1
    }
    fn run<T>(mut self, action: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        let g = self.gate.clone();
        let mut s = g.state.lock().unwrap_or_else(|e| e.into_inner());
        let id = self.id.unwrap();
        while s.serving != id && !s.closed {
            s = g.changed.wait(s).unwrap_or_else(|e| e.into_inner());
        }
        if s.closed {
            return Err("File clipboard is shutting down".into());
        }
        ferail_core::path_guard::assert_off_ui_thread("file clipboard device I/O");
        s.active += 1;
        self.id.take();
        drop(s);
        struct Completion(Arc<Gate>, u64);
        impl Drop for Completion {
            fn drop(&mut self) {
                finish(&self.0, self.1, true);
            }
        }
        let _completion = Completion(g, id);
        action()
    }
    pub fn write(
        self,
        items: &[(PathBuf, bool)],
        operation: ClipboardFileOperation,
    ) -> Result<(), String> {
        let g = self.gate.clone();
        self.run(|| {
            write_checked(&g, || {
                let refs: Vec<_> = items.iter().map(|(p, d)| (p.as_path(), *d)).collect();
                write_device(&encode(&refs, operation)?)
            })
        })
    }
    pub fn read(self) -> Result<(Vec<PathBuf>, ClipboardFileOperation), String> {
        let g = self.gate.clone();
        self.run(|| read_checked(&g))
    }
}
#[cfg(target_os = "aros")]
unsafe extern "C" {
    fn fsa_clip_write_blob(data: *const u8, length: u32) -> i32;
    fn fsa_clip_read_blob(data: *mut u8, capacity: u32, length: *mut u32) -> i32;
}
#[cfg(target_os = "aros")]
fn write_device(bytes: &[u8]) -> Result<(), String> {
    let rc = unsafe { fsa_clip_write_blob(bytes.as_ptr(), bytes.len() as u32) };
    if rc == 0 {
        Ok(())
    } else {
        Err(format!("clipboard.device write failed ({rc})"))
    }
}
#[cfg(target_os = "aros")]
fn read_device() -> Result<Vec<u8>, String> {
    let mut bytes = vec![0; MAX + 1];
    let mut length = 0;
    let rc = unsafe { fsa_clip_read_blob(bytes.as_mut_ptr(), bytes.len() as u32, &mut length) };
    if rc != 0 {
        return Err(format!("clipboard.device read failed ({rc})"));
    }
    if length as usize > MAX {
        return Err(invalid("device size"));
    }
    bytes.truncate(length as usize);
    Ok(bytes)
}
#[cfg(not(target_os = "aros"))]
fn write_device(_: &[u8]) -> Result<(), String> {
    Err("clipboard.device requires AROS".into())
}
#[cfg(not(target_os = "aros"))]
fn read_device() -> Result<Vec<u8>, String> {
    Err("clipboard.device requires AROS".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample() -> Vec<(PathBuf, bool)> {
        vec![
            (
                PathBuf::from(OsString::from_vec(b"RAM:caf\xe9.txt".to_vec())),
                false,
            ),
            (
                PathBuf::from(OsString::from_vec(b"WORK:donn\xe9es/nested\nname".to_vec())),
                true,
            ),
            (PathBuf::from("SYS:"), true),
        ]
    }
    fn bytes(op: ClipboardFileOperation) -> Vec<u8> {
        let items = sample();
        encode(
            &items
                .iter()
                .map(|(p, d)| (p.as_path(), *d))
                .collect::<Vec<_>>(),
            op,
        )
        .unwrap()
    }
    fn fsa_start(blob: &[u8]) -> usize {
        blob.windows(4).position(|b| b == b"FSAF").unwrap() + 8
    }
    fn wait<T>(future: impl std::future::Future<Output = T>) -> T {
        struct Wake(std::thread::Thread);
        impl std::task::Wake for Wake {
            fn wake(self: Arc<Self>) {
                self.0.unpark();
            }
        }
        let waker = std::task::Waker::from(Arc::new(Wake(std::thread::current())));
        let mut cx = std::task::Context::from_waker(&waker);
        let mut f = std::pin::pin!(future);
        loop {
            match f.as_mut().poll(&mut cx) {
                std::task::Poll::Ready(v) => return v,
                std::task::Poll::Pending => std::thread::park(),
            }
        }
    }
    #[test]
    fn failed_write_blocks_queued_paste_until_successful_write() {
        let g = new_gate();
        let writer = reserve_for(&g).unwrap();
        let reader = reserve_for(&g).unwrap();
        let a = g.clone();
        let bad = writer
            .submit(move || write_checked(&a, || Err("real device failure".into())))
            .unwrap();
        let a = g.clone();
        let paste = reader.submit(move || read_checked(&a)).unwrap();
        assert_eq!(wait(bad).unwrap_err(), "real device failure");
        assert!(
            wait(paste)
                .unwrap_err()
                .contains("Previous file clipboard write failed")
        );
        assert!(write_checked(&g, || Ok(())).is_ok());
        assert!(g.state.lock().unwrap().write_error.is_none());
        shutdown_for(&g).unwrap();
    }
    #[test]
    fn shutdown_cancels_queue_waits_inflight_and_joins() {
        use std::sync::mpsc;
        let g = new_gate();
        let (started, rx) = mpsc::channel();
        let (release, ready) = mpsc::channel();
        let inflight = reserve_for(&g)
            .unwrap()
            .submit(move || {
                started.send(()).unwrap();
                ready.recv().unwrap();
                Ok(17)
            })
            .unwrap();
        rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        let canceled = reserve_for(&g).unwrap();
        drop(canceled);
        let queued = reserve_for(&g)
            .unwrap()
            .submit(|| -> Result<(), String> { panic!("queued action must be canceled") })
            .unwrap();
        let outstanding = reserve_for(&g).unwrap();
        let owner = g.clone();
        let (finished, done) = mpsc::channel();
        let joiner = std::thread::spawn(move || {
            let r = shutdown_for(&owner);
            finished.send(()).unwrap();
            r
        });
        assert!(wait(queued).unwrap_err().contains("canceled"));
        assert!(reserve_for(&g).is_err());
        assert!(
            done.try_recv().is_err(),
            "shutdown must wait for actual in-flight device action"
        );
        assert!(outstanding.submit(|| Ok(())).is_err());
        release.send(()).unwrap();
        assert_eq!(wait(inflight).unwrap(), 17);
        joiner.join().unwrap().unwrap();
        done.recv().unwrap();
        let s = g.state.lock().unwrap();
        assert!(
            s.joined && s.closed && s.worker.is_none() && s.pending.is_empty() && s.active == 0
        );
        drop(s);
        shutdown_for(&g).unwrap();
        assert_eq!(
            Arc::strong_count(&g),
            1,
            "joined worker releases its service ownership"
        );
    }
    #[test]
    fn shutdown_idle_and_action_failure_leave_no_worker() {
        let g = new_gate();
        let failed = reserve_for(&g)
            .unwrap()
            .submit(|| -> Result<(), String> { Err("real error".into()) })
            .unwrap();
        assert_eq!(wait(failed).unwrap_err(), "real error");
        shutdown_for(&g).unwrap();
        assert!(reserve_for(&g).is_err());
        let g = new_gate();
        start_worker(&g).unwrap();
        shutdown_for(&g).unwrap();
        assert!(g.state.lock().unwrap().worker.is_none());
    }
    #[test]
    fn async_submission_out_of_order_and_canceled_ticket() {
        let g = new_gate();
        let first = reserve_for(&g).unwrap();
        let canceled = reserve_for(&g).unwrap();
        let second = reserve_for(&g).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let tx2 = tx.clone();
        let _later = second
            .submit(move || {
                tx2.send(2).unwrap();
                Ok(())
            })
            .unwrap();
        drop(canceled);
        let _first = first
            .submit(move || {
                tx.send(1).unwrap();
                Ok(())
            })
            .unwrap();
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap(),
            1
        );
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap(),
            2
        );
        shutdown_for(&g).unwrap();
    }
    #[test]
    fn copy_move_roundtrip_raw_dos_and_flags() {
        for op in [ClipboardFileOperation::Copy, ClipboardFileOperation::Move] {
            let blob = bytes(op);
            let (paths, got) = decode(&blob).unwrap().unwrap();
            assert_eq!(got, op);
            assert_eq!(
                paths,
                sample().into_iter().map(|(p, _)| p).collect::<Vec<_>>()
            );
            assert!(blob.windows(5).any(|b| b == b"caf\xe9."));
        }
    }
    #[test]
    fn plain_text_never_becomes_files() {
        let mut body = b"FTXT".to_vec();
        chunk(&mut body, b"CHRS", b"RAM:important\nWORK:stuff");
        let mut blob = b"FORM".to_vec();
        blob.extend_from_slice(&(body.len() as u32).to_be_bytes());
        blob.extend(body);
        assert!(decode(&blob).unwrap().is_none());
        assert!(decode(b"RAM:important\n").unwrap().is_none());
    }
    #[test]
    fn every_truncated_prefix_refuses_file_list() {
        let blob = bytes(ClipboardFileOperation::Copy);
        for n in 0..blob.len() {
            assert!(!matches!(decode(&blob[..n]), Ok(Some(_))), "{n}");
        }
    }
    #[test]
    fn bad_version_operation_count_flags_and_lengths() {
        let blob = bytes(ClipboardFileOperation::Copy);
        let at = fsa_start(&blob);
        for (offset, value) in [(at, 4), (at + 2, 9), (at + 3, 1), (at + 8, 2), (at + 9, 1)] {
            let mut b = blob.clone();
            b[offset] = value;
            assert!(decode(&b).is_err());
        }
        for range in [4..8, at + 4..at + 8, at + 12..at + 16] {
            let mut b = blob.clone();
            b[range].fill(255);
            assert!(decode(&b).is_err());
        }
    }
    #[test]
    fn malformed_padding_duplicates_and_trailing_data() {
        let blob = bytes(ClipboardFileOperation::Copy);
        let at = fsa_start(&blob);
        let mut b = blob.clone();
        chunk(&mut b, b"FSAF", &blob[at..]);
        let len = (b.len() - 8) as u32;
        b[4..8].copy_from_slice(&len.to_be_bytes());
        assert!(decode(&b).is_err());
        let mut b = blob.clone();
        b.push(0);
        assert!(decode(&b).is_err());
        let mut b = b"FORM".to_vec();
        b.extend_from_slice(&13u32.to_be_bytes());
        b.extend_from_slice(b"FTXTCHRS\0\0\0\x01x");
        assert!(decode(&b).is_err());
    }
    #[test]
    fn invalid_native_paths_and_limits() {
        for raw in [
            b"relative".as_slice(),
            b"/RAM/file",
            b":relative",
            b"dir/RAM:file",
            b"RAM:evil:name",
            b"RAM:bad\0name",
        ] {
            let path = PathBuf::from(OsString::from_vec(raw.to_vec()));
            assert!(encode(&[(&path, false)], ClipboardFileOperation::Copy).is_err());
        }
        assert!(encode(&[], ClipboardFileOperation::Copy).is_err());
        let p = Path::new("RAM:file");
        assert!(
            encode(
                &vec![(p, false); MAX_ITEMS + 1],
                ClipboardFileOperation::Copy
            )
            .is_err()
        );
        let p = PathBuf::from(OsString::from_vec(
            [b"RAM:".as_slice(), &vec![b'x'; MAX_PATH]].concat(),
        ));
        assert!(encode(&[(&p, false)], ClipboardFileOperation::Copy).is_err());
        assert!(decode(&vec![0; MAX + 1]).is_err());
    }
    #[test]
    fn bounded_mutations_do_not_panic() {
        let original = bytes(ClipboardFileOperation::Copy);
        let mut state = 0x12345678u32;
        for _ in 0..2000 {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            let mut b = original.clone();
            let at = state as usize % b.len();
            b[at] ^= (state >> 24) as u8;
            let _ = decode(&b);
        }
    }
    #[test]
    fn request_order_cancellation_and_ui_nonblocking_reservation() {
        use std::sync::{Arc, mpsc};
        use std::time::Duration;
        let g = new_gate();
        let first = reserve_for(&g).unwrap();
        let canceled = reserve_for(&g).unwrap();
        let last = reserve_for(&g).unwrap();
        drop(canceled);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let observed = seen.clone();
        let (started, rx) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            last.run(|| {
                observed.lock().unwrap().push(2);
                Ok(())
            })
        });
        let observed = seen.clone();
        first
            .run(|| {
                observed.lock().unwrap().push(1);
                let next = reserve_for(&g).unwrap();
                started.send(next).unwrap();
                Ok(())
            })
            .unwrap();
        let queued = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        thread.join().unwrap().unwrap();
        queued
            .run(|| {
                seen.lock().unwrap().push(3);
                Ok(())
            })
            .unwrap();
        assert_eq!(*seen.lock().unwrap(), vec![1, 2, 3]);
    }
}
