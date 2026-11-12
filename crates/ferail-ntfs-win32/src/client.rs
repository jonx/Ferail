use std::fmt;
use std::os::windows::ffi::OsStrExt as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock, TryLockError};
use std::time::{Duration, Instant};

use ferail_ntfs::{
    decode_frame, encode_frame, Completion, DuMessage, FailureCode, NeutralRow, Progress,
    SearchCompletion, SearchRequest, SearchRow, SizingMode, StartRequest, MAX_SEARCH_TERMS,
    MAX_SEARCH_TERM_UNITS, PROTOCOL_VERSION,
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
/// How long a cancelled request may take to reach its terminal frame before
/// the helper is terminated. Covers an uninterruptible index finish.
const CANCEL_DRAIN: Duration = Duration::from_secs(20);
const SESSION_POLL: Duration = Duration::from_millis(20);

static HELPER_SESSION: OnceLock<Mutex<Option<HelperSession>>> = OnceLock::new();
static RELEASE_REQUEST_ID: AtomicU64 = AtomicU64::new(1 << 63);

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

/// Whether a request may start the elevated helper.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HelperLaunch {
    /// Start the helper if none is running. This shows UAC: use it only
    /// behind an explicit user choice of Fast NTFS.
    MayElevate,
    /// Use the helper only if a session is already live; otherwise fail with
    /// [`ClientError::NotRunning`]. Never shows UAC.
    ReuseOnly,
}

/// What to do when another request holds the helper session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionWait {
    /// Wait for it; the wait ends early with [`ClientError::Cancelled`]
    /// when the request's cancel flag is set.
    Wait,
    /// Fail at once with [`ClientError::Busy`].
    FailIfBusy,
}

/// A non-blocking snapshot of the helper session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HelperSessionState {
    /// No elevated helper is running; a request would need UAC.
    NotStarted,
    /// A helper is connected and free.
    Idle,
    /// A helper is connected and serving another request.
    Busy,
}

#[derive(Clone, Debug)]
pub struct NtfsSearchRequest {
    pub root: PathBuf,
    /// Matched as given against the lowercased name or relative path, all
    /// of them required (`FilterExpr::text_matches`). Pass the expression's
    /// text terms, or `vec![needle.to_lowercase()]`. Empty strings are
    /// dropped; an empty list matches every visible entry.
    pub terms: Vec<String>,
    pub match_path: bool,
    pub include_hidden: bool,
    pub descend_packages: bool,
    /// Nonzero and unique per request in this process.
    pub request_id: u64,
    pub launch: HelperLaunch,
    pub wait: SessionWait,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NtfsSearchEvent {
    /// The helper validated the root and opened the volume.
    Ready,
    Progress(Progress),
    Batch(Vec<SearchRow>),
    Complete(SearchCompletion),
}

#[derive(Debug)]
pub enum ClientError {
    Cancelled,
    UacCancelled,
    HelperMissing,
    /// The helper is present but is not the binary this build shipped with:
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
    /// [`SessionWait::FailIfBusy`] and another request holds the helper.
    Busy,
    /// [`HelperLaunch::ReuseOnly`] and no helper session is live.
    NotRunning,
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
            Self::Busy => f.write_str("Fast NTFS helper is busy"),
            Self::NotRunning => f.write_str("Fast NTFS helper is not running"),
        }
    }
}

impl std::error::Error for ClientError {}

/// Non-blocking: never waits for the session lock and never touches the
/// disk. A helper that exited on its own is reported (and forgotten) as
/// `NotStarted`. Safe from any thread; do not call it per frame in render.
pub fn helper_session_state() -> HelperSessionState {
    let Some(sessions) = HELPER_SESSION.get() else {
        return HelperSessionState::NotStarted;
    };
    match sessions.try_lock() {
        Ok(mut session) => match session.as_mut() {
            None => HelperSessionState::NotStarted,
            Some(live) if live.child.has_exited() => {
                session.take();
                HelperSessionState::NotStarted
            }
            Some(_) => HelperSessionState::Idle,
        },
        Err(TryLockError::WouldBlock) => HelperSessionState::Busy,
        Err(TryLockError::Poisoned(_)) => HelperSessionState::NotStarted,
    }
}

/// Asks an idle helper to drop its cached indexes (memory only; the helper
/// keeps running). Returns [`ClientError::Busy`] while a request runs and
/// `Ok` when no helper is running.
pub fn release_ntfs_index() -> Result<(), ClientError> {
    let never = never_cancelled();
    let mut session = lock_session(&never, SessionWait::FailIfBusy)?;
    let Some(live) = session.as_mut() else {
        return Ok(());
    };
    let request_id = RELEASE_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
    let frame = encode_frame(request_id, &DuMessage::Release)
        .map_err(|_| ClientError::Protocol("Release encode"))?;
    if let Err(error) =
        live.server
            .pipe
            .write_frame(&frame, Instant::now() + INACTIVITY_TIMEOUT, &never)
    {
        session.take();
        return Err(map_pipe_io(error));
    }
    Ok(())
}

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

    let mut stream = StreamValidator::new(start.root_id, start.first_child_id);
    with_session(
        cancel,
        HelperLaunch::MayElevate,
        SessionWait::Wait,
        |session| {
            session.exchange(
                request.request_id,
                &DuMessage::Start(start),
                cancel,
                |message, ready| match message {
                    DuMessage::Ready if !ready => {
                        on_event(FastNtfsEvent::Ready);
                        Ok(Flow::Ready)
                    }
                    DuMessage::Batch(rows) if ready => {
                        stream.accept_batch(&rows)?;
                        on_event(FastNtfsEvent::Batch(rows));
                        Ok(Flow::Continue)
                    }
                    DuMessage::Progress(progress) if ready => {
                        on_event(FastNtfsEvent::Progress(progress));
                        Ok(Flow::Continue)
                    }
                    DuMessage::Complete(complete) if ready => {
                        stream.accept_complete(complete)?;
                        on_event(FastNtfsEvent::Complete(complete));
                        Ok(Flow::Done)
                    }
                    _ => Err(ClientError::Protocol("message order")),
                },
            )
        },
    )
}

/// Runs a name search through the elevated helper's shared MFT index. Must
/// run off the UI thread (it probes the root path and blocks on the pipe).
///
/// - [`ClientError::NotRunning`]: `ReuseOnly` and no helper; use the walker.
/// - [`ClientError::Busy`]: `FailIfBusy` and the helper is serving another
///   request (a Disk Usage scan or another search); use the walker.
/// - [`ClientError::Helper`]`(Unsupported | Validation | ..)`, `Platform`,
///   `Protocol`, `Timeout`: Fast NTFS failed; fall back to the walker. No
///   rows from a failed search may be kept.
/// - [`ClientError::Cancelled`]: the cancel flag was set; no fallback.
pub fn run_ntfs_search(
    request: NtfsSearchRequest,
    cancel: &AtomicBool,
    mut on_event: impl FnMut(NtfsSearchEvent),
) -> Result<(), ClientError> {
    if cancel.load(Ordering::Acquire) {
        return Err(ClientError::Cancelled);
    }
    if request.request_id == 0 {
        return Err(ClientError::Protocol("request identity"));
    }
    let terms = search_terms(&request.terms)?;
    if request.launch == HelperLaunch::ReuseOnly
        && helper_session_state() == HelperSessionState::NotStarted
    {
        return Err(ClientError::NotRunning);
    }
    let probe = probe_fast_ntfs(&request.root).map_err(|_| ClientError::Platform("probe"))?;
    let (root_identity, _) =
        file_identity(&request.root).map_err(|_| ClientError::Platform("root identity"))?;
    let search = SearchRequest {
        volume_guid: probe.volume_guid,
        root: request.root.as_os_str().encode_wide().collect(),
        root_identity,
        terms,
        match_path: request.match_path,
        include_hidden: request.include_hidden,
        descend_packages: request.descend_packages,
    };

    let mut stream = SearchStreamValidator::default();
    with_session(cancel, request.launch, request.wait, |session| {
        session.exchange(
            request.request_id,
            &DuMessage::Search(search),
            cancel,
            |message, ready| match message {
                DuMessage::Ready if !ready => {
                    on_event(NtfsSearchEvent::Ready);
                    Ok(Flow::Ready)
                }
                DuMessage::SearchBatch(rows) if ready => {
                    stream.accept_batch(&rows)?;
                    on_event(NtfsSearchEvent::Batch(rows));
                    Ok(Flow::Continue)
                }
                DuMessage::Progress(progress) if ready => {
                    on_event(NtfsSearchEvent::Progress(progress));
                    Ok(Flow::Continue)
                }
                DuMessage::SearchComplete(complete) if ready => {
                    stream.accept_complete(complete)?;
                    on_event(NtfsSearchEvent::Complete(complete));
                    Ok(Flow::Done)
                }
                _ => Err(ClientError::Protocol("message order")),
            },
        )
    })
}

fn search_terms(terms: &[String]) -> Result<Vec<String>, ClientError> {
    let terms: Vec<String> = terms.iter().filter(|t| !t.is_empty()).cloned().collect();
    if terms.len() > MAX_SEARCH_TERMS
        || terms
            .iter()
            .any(|term| term.encode_utf16().count() > MAX_SEARCH_TERM_UNITS || term.contains('\0'))
    {
        return Err(ClientError::Protocol("search terms"));
    }
    Ok(terms)
}

fn lock_session(
    cancel: &AtomicBool,
    wait: SessionWait,
) -> Result<MutexGuard<'static, Option<HelperSession>>, ClientError> {
    let sessions = HELPER_SESSION.get_or_init(|| Mutex::new(None));
    loop {
        match sessions.try_lock() {
            Ok(session) => return Ok(session),
            Err(TryLockError::Poisoned(poisoned)) => {
                // A panic while holding the session leaves its protocol state
                // unknown: forget the connection, keep the lock usable.
                let mut session = poisoned.into_inner();
                session.take();
                sessions.clear_poison();
                return Ok(session);
            }
            Err(TryLockError::WouldBlock) => {
                if wait == SessionWait::FailIfBusy {
                    return Err(ClientError::Busy);
                }
                if cancel.load(Ordering::Acquire) {
                    return Err(ClientError::Cancelled);
                }
                std::thread::sleep(SESSION_POLL);
            }
        }
    }
}

/// Locks the session, connects (only if `launch` allows it), runs `body`,
/// and forgets the connection when `body` left it in an unknown state.
fn with_session(
    cancel: &AtomicBool,
    launch: HelperLaunch,
    wait: SessionWait,
    body: impl FnOnce(&mut HelperSession) -> Result<(), ExchangeError>,
) -> Result<(), ClientError> {
    let mut session = lock_session(cancel, wait)?;
    if cancel.load(Ordering::Acquire) {
        return Err(ClientError::Cancelled);
    }
    if session.as_mut().is_some_and(|live| live.child.has_exited()) {
        session.take();
    }
    if session.is_none() {
        if launch == HelperLaunch::ReuseOnly {
            return Err(ClientError::NotRunning);
        }
        *session = Some(HelperSession::connect(cancel)?);
    }
    match body(session.as_mut().expect("session inserted")) {
        Ok(()) => Ok(()),
        Err(ExchangeError { error, reusable }) => {
            if !reusable {
                // Unread protocol bytes or an unknown helper state: drop
                // this connection; a later explicit attempt may start anew.
                session.take();
            }
            Err(error)
        }
    }
}

enum Flow {
    Ready,
    Continue,
    Done,
}

struct ExchangeError {
    error: ClientError,
    /// The helper reached a clean terminal frame and awaits the next
    /// request, so the session (and its cached index) can be kept.
    reusable: bool,
}

impl From<ClientError> for ExchangeError {
    fn from(error: ClientError) -> Self {
        Self {
            error,
            reusable: false,
        }
    }
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

    /// Sends one request and feeds every reply to `handle` until it returns
    /// [`Flow::Done`]. `handle` receives `ready` = whether Ready was seen.
    fn exchange(
        &mut self,
        request_id: u64,
        request: &DuMessage,
        cancel: &AtomicBool,
        mut handle: impl FnMut(DuMessage, bool) -> Result<Flow, ClientError>,
    ) -> Result<(), ExchangeError> {
        let frame = encode_frame(request_id, request)
            .map_err(|_| ClientError::Protocol("request encode"))?;
        // Written whole even if `cancel` is set meanwhile; the read loop
        // below then cancels and drains cleanly.
        if let Err(error) = self.server.pipe.write_frame(
            &frame,
            Instant::now() + INACTIVITY_TIMEOUT,
            &never_cancelled(),
        ) {
            // A partially written request leaves the stream unusable.
            self.child.terminate();
            return Err(map_pipe_io(error).into());
        }

        let absolute_deadline = Instant::now() + ABSOLUTE_TIMEOUT;
        let mut ready = false;
        loop {
            if Instant::now() >= absolute_deadline {
                self.cancel_and_drain(request_id);
                return Err(ClientError::Timeout("absolute request deadline").into());
            }
            let deadline = (Instant::now() + INACTIVITY_TIMEOUT).min(absolute_deadline);
            let frame = match self.server.pipe.read_frame(deadline, cancel) {
                Ok(frame) => frame,
                Err(PipeError::Cancelled) => {
                    let reusable = self.cancel_and_drain(request_id);
                    return Err(ExchangeError {
                        error: ClientError::Cancelled,
                        reusable,
                    });
                }
                Err(error) => {
                    self.child.terminate();
                    return Err(map_pipe_io(error).into());
                }
            };
            let message = match decode_frame(&frame, Some(request_id)) {
                Ok((_, message)) => message,
                Err(_) => {
                    self.child.terminate();
                    return Err(ClientError::Protocol("event decode").into());
                }
            };
            if let DuMessage::Failed(code) = message {
                // A clean terminal frame: the helper waits for the next
                // request with its cache intact.
                return Err(ExchangeError {
                    error: ClientError::Helper(code),
                    reusable: true,
                });
            }
            match handle(message, ready) {
                Ok(Flow::Ready) => ready = true,
                Ok(Flow::Continue) => {}
                Ok(Flow::Done) => return Ok(()),
                Err(error) => {
                    self.child.terminate();
                    return Err(error.into());
                }
            }
        }
    }

    /// Sends Cancel and reads until the request's terminal frame so the
    /// helper (and its cached index) survives the cancellation. Returns
    /// whether that worked; otherwise the helper is terminated.
    fn cancel_and_drain(&mut self, request_id: u64) -> bool {
        let never = never_cancelled();
        let sent = encode_frame(request_id, &DuMessage::Cancel)
            .ok()
            .is_some_and(|frame| {
                self.server
                    .pipe
                    .write_frame(&frame, Instant::now() + Duration::from_secs(2), &never)
                    .is_ok()
            });
        if sent {
            let deadline = Instant::now() + CANCEL_DRAIN;
            while let Ok(frame) = self.server.pipe.read_frame(deadline, &never) {
                match decode_frame(&frame, Some(request_id)) {
                    Ok((
                        _,
                        DuMessage::Failed(_)
                        | DuMessage::Complete(_)
                        | DuMessage::SearchComplete(_),
                    )) => return true,
                    Ok((
                        _,
                        DuMessage::Ready
                        | DuMessage::Progress(_)
                        | DuMessage::Batch(_)
                        | DuMessage::SearchBatch(_),
                    )) => continue,
                    _ => break,
                }
            }
        }
        if !self.child.wait(CANCEL_GRACE) {
            self.child.terminate();
        }
        false
    }
}

#[derive(Default)]
struct SearchStreamValidator {
    rows: u64,
}

impl SearchStreamValidator {
    fn accept_batch(&mut self, rows: &[SearchRow]) -> Result<(), ClientError> {
        if rows.is_empty() || rows.len() > 256 {
            return Err(ClientError::Protocol("search batch row count"));
        }
        // The decoder already refused paths that could leave the root.
        if !rows
            .iter()
            .all(|row| ferail_ntfs::valid_relative_path(&row.relative_path))
        {
            return Err(ClientError::Protocol("search row path"));
        }
        self.rows = self
            .rows
            .checked_add(rows.len() as u64)
            .ok_or(ClientError::Protocol("search row count overflow"))?;
        Ok(())
    }

    fn accept_complete(&self, complete: SearchCompletion) -> Result<(), ClientError> {
        if complete.matches != self.rows {
            return Err(ClientError::Protocol("search completion totals"));
        }
        Ok(())
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

    /// Non-blocking liveness check.
    fn has_exited(&self) -> bool {
        (unsafe { WaitForSingleObject(self.handle, 0) }) == WAIT_OBJECT_0
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
        assert_eq!(parameters, format!("2 {pipe}"));
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
    fn search_terms_drop_empty_and_respect_limits() {
        assert_eq!(
            search_terms(&["a".into(), String::new()]).unwrap(),
            vec!["a".to_string()]
        );
        assert!(search_terms(&vec!["a".into(); MAX_SEARCH_TERMS + 1]).is_err());
        assert!(search_terms(&["x".repeat(MAX_SEARCH_TERM_UNITS + 1)]).is_err());
        assert!(search_terms(&["a\0b".into()]).is_err());
    }

    #[test]
    fn search_validator_checks_match_count() {
        let row = SearchRow {
            relative_path: vec![b'x' as u16],
            file_record: ferail_ntfs::FileReference {
                record: 20,
                sequence: 1,
            },
            kind: ferail_ntfs::SearchEntryKind::File,
            cloud: false,
            attributes: 0,
            logical_bytes: 0,
            allocated_bytes: 0,
            modified_ticks: 0,
            created_ticks: 0,
        };
        let mut validator = SearchStreamValidator::default();
        validator.accept_batch(&[row.clone(), row]).unwrap();
        let complete = SearchCompletion {
            matches: 2,
            entries_visited: 2,
            directories_descended: 0,
            skipped_paths: 0,
            cycles_skipped: 0,
            corrupt_records: 0,
            start_journal_id: 0,
            start_next_usn: 0,
            end_journal_id: 0,
            end_next_usn: 0,
            index_reused: false,
            best_effort_live: false,
        };
        assert!(validator.accept_complete(complete).is_ok());
        assert!(validator
            .accept_complete(SearchCompletion {
                matches: 3,
                ..complete
            })
            .is_err());
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
