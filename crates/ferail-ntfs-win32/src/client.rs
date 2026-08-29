use std::fmt;
use std::os::windows::ffi::OsStrExt as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use ferail_ntfs::{
    decode_frame, encode_frame, Completion, DuMessage, FailureCode, NeutralRow, Progress,
    SizingMode, StartRequest, PROTOCOL_VERSION,
};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, ERROR_CANCELLED, HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::Pipes::GetNamedPipeClientProcessId;
use windows::Win32::System::Threading::{GetProcessId, TerminateProcess, WaitForSingleObject};
use windows::Win32::UI::Shell::{
    ShellExecuteExW, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SEE_MASK_NO_CONSOLE,
    SHELLEXECUTEINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

use crate::pipe::{never_cancelled, PipeError, PipeServer};
use crate::{file_identity, probe_fast_ntfs};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(120);
const INACTIVITY_TIMEOUT: Duration = Duration::from_secs(30);
const ABSOLUTE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const CANCEL_GRACE: Duration = Duration::from_secs(5);

static HELPER_SESSION: OnceLock<Mutex<Option<HelperSession>>> = OnceLock::new();

#[derive(Clone, Debug)]
pub struct FastNtfsRequest {
    pub root: PathBuf,
    pub sizing_mode: SizingMode,
    pub descend_packages: bool,
    pub root_id: u64,
    pub first_child_id: u64,
    pub request_id: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FastNtfsEvent {
    Ready,
    Batch(Vec<NeutralRow>),
    Progress(Progress),
    Complete(Completion),
}

#[derive(Debug)]
pub enum ClientError {
    Cancelled,
    UacCancelled,
    HelperMissing,
    /// The helper is present but is not the binary this build shipped with —
    /// a stale copy from an older version, a partial update, or a
    /// substitution. Fails closed into the Portable engine. See
    /// [`crate::attest`].
    HelperUntrusted,
    /// The helper is present but could not be opened for verification (an
    /// unreadable file, or something holding it against our read).
    HelperUnreadable,
    Timeout(&'static str),
    Protocol(&'static str),
    Helper(FailureCode),
    Platform(&'static str),
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => f.write_str("Fast NTFS cancelled"),
            Self::UacCancelled => f.write_str("Fast NTFS elevation was declined"),
            Self::HelperMissing => f.write_str("Fast NTFS helper is missing"),
            Self::HelperUntrusted => f.write_str("Fast NTFS helper failed its integrity check"),
            Self::HelperUnreadable => {
                f.write_str("Fast NTFS helper could not be opened for verification")
            }
            Self::Timeout(phase) => write!(f, "Fast NTFS timed out during {phase}"),
            Self::Protocol(phase) => write!(f, "Fast NTFS protocol error during {phase}"),
            Self::Helper(code) => write!(f, "Fast NTFS helper failed: {code:?}"),
            Self::Platform(phase) => write!(f, "Fast NTFS platform failure during {phase}"),
        }
    }
}

impl std::error::Error for ClientError {}

pub fn run_fast_ntfs(
    request: FastNtfsRequest,
    cancel: &AtomicBool,
    mut on_event: impl FnMut(FastNtfsEvent),
) -> Result<(), ClientError> {
    if cancel.load(Ordering::Acquire) {
        return Err(ClientError::Cancelled);
    }
    if request.request_id == 0
        || request.root_id == 0
        || request.first_child_id != request.root_id.checked_add(1).unwrap_or(0)
    {
        return Err(ClientError::Protocol("request identity namespace"));
    }
    let probe = probe_fast_ntfs(&request.root).map_err(|_| ClientError::Platform("probe"))?;
    let (root_identity, _) =
        file_identity(&request.root).map_err(|_| ClientError::Platform("root identity"))?;
    let start = StartRequest {
        volume_guid: probe.volume_guid,
        root: request.root.as_os_str().encode_wide().collect(),
        root_identity,
        sizing_mode: request.sizing_mode,
        descend_packages: request.descend_packages,
        root_id: request.root_id,
        first_child_id: request.first_child_id,
    };

    let sessions = HELPER_SESSION.get_or_init(|| Mutex::new(None));
    let mut session = sessions
        .lock()
        .map_err(|_| ClientError::Platform("helper session lock"))?;
    if cancel.load(Ordering::Acquire) {
        return Err(ClientError::Cancelled);
    }
    if session.is_none() {
        *session = Some(HelperSession::connect(cancel)?);
    }
    let result = session.as_mut().expect("session inserted").scan(
        request.request_id,
        start,
        cancel,
        &mut on_event,
    );
    if result.is_err() {
        // A terminal error may leave unread protocol bytes or an unknown
        // helper state. Drop this connection; a later explicit attempt may
        // establish a fresh elevated session.
        session.take();
    }
    result
}

struct HelperSession {
    server: PipeServer,
    child: ChildProcess,
}

impl HelperSession {
    fn connect(cancel: &AtomicBool) -> Result<Self, ClientError> {
        let server = PipeServer::create().map_err(map_pipe_start)?;
        let mut child = ChildProcess::launch(&server.name)?;
        server
            .connect(Instant::now() + CONNECT_TIMEOUT, cancel)
            .map_err(map_pipe_connect)?;
        let mut pipe_pid = 0u32;
        unsafe { GetNamedPipeClientProcessId(server.pipe.handle(), &mut pipe_pid) }
            .map_err(|_| ClientError::Platform("pipe client identity"))?;
        if pipe_pid == 0 || pipe_pid != child.pid {
            child.terminate();
            return Err(ClientError::Protocol("pipe PID authentication"));
        }

        let hello_frame = server
            .pipe
            .read_frame(Instant::now() + INACTIVITY_TIMEOUT, cancel)
            .map_err(map_pipe_io)?;
        let (hello_request, hello) =
            decode_frame(&hello_frame, None).map_err(|_| ClientError::Protocol("Hello"))?;
        if hello_request != 0
            || hello
                != (DuMessage::Hello {
                    helper_pid: child.pid,
                })
        {
            child.terminate();
            return Err(ClientError::Protocol("Hello identity"));
        }
        Ok(Self { server, child })
    }

    fn scan(
        &mut self,
        request_id: u64,
        start: StartRequest,
        cancel: &AtomicBool,
        on_event: &mut impl FnMut(FastNtfsEvent),
    ) -> Result<(), ClientError> {
        let mut stream = StreamValidator::new(start.root_id, start.first_child_id);
        let start_frame = encode_frame(request_id, &DuMessage::Start(start))
            .map_err(|_| ClientError::Protocol("Start encode"))?;
        self.server
            .pipe
            .write_frame(&start_frame, Instant::now() + INACTIVITY_TIMEOUT, cancel)
            .map_err(map_pipe_io)?;

        let absolute_deadline = Instant::now() + ABSOLUTE_TIMEOUT;
        let mut ready = false;
        loop {
            if Instant::now() >= absolute_deadline {
                cancel_helper(&self.server, &mut self.child, request_id);
                return Err(ClientError::Timeout("absolute scan deadline"));
            }
            let deadline = (Instant::now() + INACTIVITY_TIMEOUT).min(absolute_deadline);
            let frame = match self.server.pipe.read_frame(deadline, cancel) {
                Ok(frame) => frame,
                Err(PipeError::Cancelled) => {
                    cancel_helper(&self.server, &mut self.child, request_id);
                    return Err(ClientError::Cancelled);
                }
                Err(error) => {
                    self.child.terminate();
                    return Err(map_pipe_io(error));
                }
            };
            let (_, message) = decode_frame(&frame, Some(request_id))
                .map_err(|_| ClientError::Protocol("event decode"))?;
            match message {
                DuMessage::Ready if !ready => {
                    ready = true;
                    on_event(FastNtfsEvent::Ready);
                }
                DuMessage::Batch(rows) if ready => {
                    stream.accept_batch(&rows)?;
                    on_event(FastNtfsEvent::Batch(rows));
                }
                DuMessage::Progress(progress) if ready => {
                    on_event(FastNtfsEvent::Progress(progress));
                }
                DuMessage::Complete(complete) if ready => {
                    stream.accept_complete(complete)?;
                    on_event(FastNtfsEvent::Complete(complete));
                    return Ok(());
                }
                DuMessage::Failed(code) => return Err(ClientError::Helper(code)),
                _ => {
                    self.child.terminate();
                    return Err(ClientError::Protocol("message order"));
                }
            }
        }
    }
}

struct StreamValidator {
    root_id: u64,
    next_id: u64,
    rows: u64,
    logical_bytes: u64,
    allocated_bytes: u64,
    containers: Vec<bool>,
}

impl StreamValidator {
    fn new(root_id: u64, first_child_id: u64) -> Self {
        Self {
            root_id,
            next_id: first_child_id,
            rows: 0,
            logical_bytes: 0,
            allocated_bytes: 0,
            containers: vec![true],
        }
    }

    fn accept_batch(&mut self, rows: &[NeutralRow]) -> Result<(), ClientError> {
        if rows.is_empty() || rows.len() > 256 {
            return Err(ClientError::Protocol("batch row count"));
        }
        for row in rows {
            let parent_index = row.parent_id.checked_sub(self.root_id).and_then(|offset| {
                usize::try_from(offset)
                    .ok()
                    .filter(|index| self.containers.get(*index) == Some(&true))
            });
            if row.id != self.next_id
                || row.parent_id < self.root_id
                || row.parent_id >= row.id
                || parent_index.is_none()
            {
                return Err(ClientError::Protocol("batch identity ordering"));
            }
            self.next_id = self
                .next_id
                .checked_add(1)
                .ok_or(ClientError::Protocol("batch identity overflow"))?;
            self.rows = self
                .rows
                .checked_add(1)
                .ok_or(ClientError::Protocol("row count overflow"))?;
            self.logical_bytes = self
                .logical_bytes
                .checked_add(row.logical_bytes)
                .ok_or(ClientError::Protocol("logical byte total overflow"))?;
            self.allocated_bytes = self
                .allocated_bytes
                .checked_add(row.allocated_bytes)
                .ok_or(ClientError::Protocol("allocated byte total overflow"))?;
            self.containers
                .push(row.kind == ferail_ntfs::NeutralNodeKind::Directory);
        }
        Ok(())
    }

    fn accept_complete(&self, complete: Completion) -> Result<(), ClientError> {
        if complete.rows != self.rows
            || complete.logical_bytes != self.logical_bytes
            || complete.allocated_bytes != self.allocated_bytes
        {
            return Err(ClientError::Protocol("completion totals"));
        }
        Ok(())
    }
}

struct ChildProcess {
    handle: HANDLE,
    pid: u32,
    terminate_on_drop: bool,
}

// SAFETY: process handles may be waited on or terminated from any thread, and
// every access to a cached ChildProcess is serialized by HELPER_SESSION.
unsafe impl Send for ChildProcess {}

impl ChildProcess {
    fn launch(pipe_name: &str) -> Result<Self, ClientError> {
        let mut helper = std::env::current_exe().map_err(|_| ClientError::HelperMissing)?;
        helper.set_file_name("ferail-ntfs-helper.exe");
        if !helper.is_file() {
            return Err(ClientError::HelperMissing);
        }
        // Verify the helper we are about to elevate, and keep its handle open
        // across ShellExecuteExW: the open denies writers and deleters, so
        // the bytes Windows maps are provably the bytes we just hashed. This
        // is the part of `attest` that is a real guarantee rather than a cost
        // increase, so the guard must outlive the launch call below.
        let held = match crate::attest::open_verified(&helper) {
            Ok(held) => held,
            Err(crate::attest::AttestError::Mismatch) => return Err(ClientError::HelperUntrusted),
            Err(crate::attest::AttestError::Unreadable(_)) => {
                return Err(ClientError::HelperUnreadable);
            }
        };
        let parameters = format!("{} {}", PROTOCOL_VERSION, pipe_name);
        let helper_w: Vec<u16> = helper.as_os_str().encode_wide().chain(Some(0)).collect();
        let parameters_w: Vec<u16> = parameters.encode_utf16().chain(Some(0)).collect();
        let verb_w: Vec<u16> = "runas".encode_utf16().chain(Some(0)).collect();
        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NO_CONSOLE | SEE_MASK_NOASYNC,
            lpVerb: PCWSTR(verb_w.as_ptr()),
            lpFile: PCWSTR(helper_w.as_ptr()),
            lpParameters: PCWSTR(parameters_w.as_ptr()),
            nShow: SW_HIDE.0,
            ..Default::default()
        };
        unsafe { ShellExecuteExW(&mut info) }.map_err(|error| {
            if error.code() == ERROR_CANCELLED.to_hresult() {
                ClientError::UacCancelled
            } else {
                ClientError::Platform("launch helper")
            }
        })?;
        if info.hProcess.is_invalid() {
            return Err(ClientError::Platform("missing helper process handle"));
        }
        // Windows has mapped the image by now, so the deny-write hold has
        // done its job. Released explicitly rather than at end of scope so the
        // ordering against ShellExecuteExW above stays visible.
        drop(held);
        let pid = unsafe { GetProcessId(info.hProcess) };
        if pid == 0 {
            unsafe {
                let _ = CloseHandle(info.hProcess);
            }
            return Err(ClientError::Platform("helper PID"));
        }
        Ok(Self {
            handle: info.hProcess,
            pid,
            terminate_on_drop: true,
        })
    }

    fn wait(&mut self, timeout: Duration) -> bool {
        let milliseconds = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
        let exited = (unsafe { WaitForSingleObject(self.handle, milliseconds) }) == WAIT_OBJECT_0;
        if exited {
            self.terminate_on_drop = false;
        }
        exited
    }

    fn terminate(&mut self) {
        unsafe {
            let _ = TerminateProcess(self.handle, 1);
            let _ = WaitForSingleObject(self.handle, CANCEL_GRACE.as_millis() as u32);
        }
        self.terminate_on_drop = false;
    }
}

impl Drop for ChildProcess {
    fn drop(&mut self) {
        unsafe {
            if self.terminate_on_drop {
                let _ = TerminateProcess(self.handle, 1);
                let _ = WaitForSingleObject(self.handle, CANCEL_GRACE.as_millis() as u32);
            }
            let _ = CloseHandle(self.handle);
        }
    }
}

fn cancel_helper(server: &PipeServer, child: &mut ChildProcess, request_id: u64) {
    let cancel = never_cancelled();
    if let Ok(frame) = encode_frame(request_id, &DuMessage::Cancel) {
        let _ = server
            .pipe
            .write_frame(&frame, Instant::now() + Duration::from_secs(2), &cancel);
    }
    if !child.wait(CANCEL_GRACE) {
        child.terminate();
    }
}

fn map_pipe_start(error: PipeError) -> ClientError {
    match error {
        PipeError::Timeout => ClientError::Timeout("pipe creation"),
        PipeError::Cancelled => ClientError::Cancelled,
        _ => ClientError::Platform("pipe creation"),
    }
}

fn map_pipe_connect(error: PipeError) -> ClientError {
    match error {
        PipeError::Timeout => ClientError::Timeout("helper connection"),
        PipeError::Cancelled => ClientError::Cancelled,
        _ => ClientError::Platform("helper connection"),
    }
}

fn map_pipe_io(error: PipeError) -> ClientError {
    match error {
        PipeError::Timeout => ClientError::Timeout("pipe inactivity"),
        PipeError::Cancelled => ClientError::Cancelled,
        PipeError::InvalidFrame => ClientError::Protocol("frame bounds"),
        PipeError::Disconnected | PipeError::Win32(_, _) => {
            ClientError::Platform("private pipe I/O")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_parameters_contain_no_requested_path() {
        let pipe = r"\\.\pipe\Ferail.FastNtfs.0123456789abcdef";
        let parameters = format!("{} {}", PROTOCOL_VERSION, pipe);
        assert_eq!(parameters, format!("1 {pipe}"));
        assert!(!parameters.contains(":"));
    }

    #[test]
    fn timeout_wait_result_is_not_success() {
        assert_ne!(windows::Win32::Foundation::WAIT_TIMEOUT, WAIT_OBJECT_0);
    }

    fn row(id: u64, parent_id: u64) -> NeutralRow {
        NeutralRow {
            id,
            parent_id,
            file_record: ferail_ntfs::FileReference {
                record: id,
                sequence: 1,
            },
            kind: ferail_ntfs::NeutralNodeKind::File,
            raw_name: vec![b'x' as u16],
            display_name: "x".into(),
            logical_bytes: 3,
            allocated_bytes: 4,
            modified_ticks: 0,
        }
    }

    #[test]
    fn stream_validator_rejects_sparse_or_forward_ids() {
        let mut validator = StreamValidator::new(10, 11);
        assert!(validator.accept_batch(&[row(12, 10)]).is_err());

        let mut validator = StreamValidator::new(10, 11);
        assert!(validator.accept_batch(&[row(11, 12)]).is_err());
    }

    #[test]
    fn stream_validator_checks_terminal_totals() {
        let mut validator = StreamValidator::new(10, 11);
        validator.accept_batch(&[row(11, 10), row(12, 10)]).unwrap();
        let valid = Completion {
            rows: 2,
            logical_bytes: 6,
            allocated_bytes: 8,
            corrupt_records: 0,
            skipped_records: 0,
            start_journal_id: 0,
            start_next_usn: 0,
            end_journal_id: 0,
            end_next_usn: 0,
            best_effort_live: false,
        };
        assert!(validator.accept_complete(valid).is_ok());
        assert!(validator
            .accept_complete(Completion { rows: 3, ..valid })
            .is_err());
    }
}
