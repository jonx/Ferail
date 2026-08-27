use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ferail_ntfs::{FRAME_HEADER_BYTES, MAX_FRAME_BYTES};
use uuid::Uuid;
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, LocalFree, ERROR_IO_PENDING, ERROR_PIPE_CONNECTED, GENERIC_READ, GENERIC_WRITE,
    HANDLE, HLOCAL, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    GetTokenInformation, TokenUser, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY,
    TOKEN_USER,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED,
    FILE_SHARE_MODE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, NAMED_PIPE_MODE, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, OpenProcessToken, WaitForSingleObject,
};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

const PIPE_BUFFER_BYTES: u32 = 64 * 1024;
const WAIT_SLICE: Duration = Duration::from_millis(100);

#[derive(Debug)]
pub(crate) enum PipeError {
    Win32(&'static str, windows::core::Error),
    Timeout,
    Cancelled,
    Disconnected,
    InvalidFrame,
}

impl fmt::Display for PipeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Win32(context, error) => write!(f, "{context}: {error}"),
            Self::Timeout => f.write_str("private pipe timeout"),
            Self::Cancelled => f.write_str("private pipe cancelled"),
            Self::Disconnected => f.write_str("private pipe disconnected"),
            Self::InvalidFrame => f.write_str("invalid private pipe frame"),
        }
    }
}

pub(crate) struct Pipe {
    handle: Arc<OwnedHandle>,
}

impl Clone for Pipe {
    fn clone(&self) -> Self {
        Self {
            handle: self.handle.clone(),
        }
    }
}

impl Pipe {
    pub(crate) fn from_handle(handle: HANDLE) -> Self {
        Self {
            handle: Arc::new(OwnedHandle(handle)),
        }
    }

    pub(crate) fn handle(&self) -> HANDLE {
        self.handle.0
    }

    pub(crate) fn read_frame(
        &self,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> Result<Vec<u8>, PipeError> {
        let mut header = [0u8; FRAME_HEADER_BYTES];
        self.read_exact(&mut header, deadline, cancel)?;
        let payload_length = u32::from_le_bytes(
            header[16..20]
                .try_into()
                .map_err(|_| PipeError::InvalidFrame)?,
        ) as usize;
        if payload_length > MAX_FRAME_BYTES {
            return Err(PipeError::InvalidFrame);
        }
        let frame_length = FRAME_HEADER_BYTES
            .checked_add(payload_length)
            .ok_or(PipeError::InvalidFrame)?;
        let mut frame = Vec::with_capacity(frame_length);
        frame.extend_from_slice(&header);
        frame.resize(frame_length, 0);
        self.read_exact(&mut frame[FRAME_HEADER_BYTES..], deadline, cancel)?;
        Ok(frame)
    }

    pub(crate) fn write_frame(
        &self,
        frame: &[u8],
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> Result<(), PipeError> {
        if frame.len() < FRAME_HEADER_BYTES || frame.len() > FRAME_HEADER_BYTES + MAX_FRAME_BYTES {
            return Err(PipeError::InvalidFrame);
        }
        let mut written = 0usize;
        while written < frame.len() {
            let count = overlapped_write(self.handle.0, &frame[written..], deadline, cancel)?;
            if count == 0 {
                return Err(PipeError::Disconnected);
            }
            written = written.checked_add(count).ok_or(PipeError::InvalidFrame)?;
        }
        Ok(())
    }

    pub(crate) fn cancel_all(&self) {
        unsafe {
            let _ = CancelIoEx(self.handle.0, None);
        }
    }

    fn read_exact(
        &self,
        destination: &mut [u8],
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> Result<(), PipeError> {
        let mut read_total = 0usize;
        while read_total < destination.len() {
            let count = overlapped_read(
                self.handle.0,
                &mut destination[read_total..],
                deadline,
                cancel,
            )?;
            if count == 0 {
                return Err(PipeError::Disconnected);
            }
            read_total = read_total
                .checked_add(count)
                .ok_or(PipeError::InvalidFrame)?;
        }
        Ok(())
    }
}

pub(crate) struct PipeServer {
    pub(crate) pipe: Pipe,
    pub(crate) name: String,
}

impl PipeServer {
    pub(crate) fn create() -> Result<Self, PipeError> {
        let name = format!(r"\\.\pipe\Ferail.FastNtfs.{}", Uuid::new_v4().simple());
        let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        let descriptor = SecurityDescriptor::for_current_user()?;
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0 .0,
            bInheritHandle: false.into(),
        };
        let open_mode = windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES(
            PIPE_ACCESS_DUPLEX.0 | FILE_FLAG_FIRST_PIPE_INSTANCE.0 | FILE_FLAG_OVERLAPPED.0,
        );
        let pipe_mode = NAMED_PIPE_MODE(
            PIPE_TYPE_BYTE.0 | PIPE_READMODE_BYTE.0 | PIPE_WAIT.0 | PIPE_REJECT_REMOTE_CLIENTS.0,
        );
        let handle = unsafe {
            CreateNamedPipeW(
                PCWSTR(wide.as_ptr()),
                open_mode,
                pipe_mode,
                1,
                PIPE_BUFFER_BYTES,
                PIPE_BUFFER_BYTES,
                0,
                Some(&attributes),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(PipeError::Win32(
                "CreateNamedPipeW",
                windows::core::Error::from_win32(),
            ));
        }
        Ok(Self {
            pipe: Pipe::from_handle(handle),
            name,
        })
    }

    pub(crate) fn connect(&self, deadline: Instant, cancel: &AtomicBool) -> Result<(), PipeError> {
        let event = Event::new()?;
        let mut overlapped = OVERLAPPED {
            hEvent: event.0,
            ..Default::default()
        };
        match unsafe { ConnectNamedPipe(self.pipe.handle(), Some(&mut overlapped)) } {
            Ok(()) => Ok(()),
            Err(error) if error.code() == ERROR_PIPE_CONNECTED.to_hresult() => Ok(()),
            Err(error) if error.code() == ERROR_IO_PENDING.to_hresult() => {
                wait_overlapped(self.pipe.handle(), &overlapped, event.0, deadline, cancel)?;
                Ok(())
            }
            Err(error) => Err(PipeError::Win32("ConnectNamedPipe", error)),
        }
    }
}

impl Drop for PipeServer {
    fn drop(&mut self) {
        unsafe {
            let _ = DisconnectNamedPipe(self.pipe.handle());
        }
    }
}

struct OwnedHandle(HANDLE);

// SAFETY: all operations use independent OVERLAPPED structures; the kernel
// supports simultaneous full-duplex named-pipe reads and writes.
unsafe impl Send for OwnedHandle {}
unsafe impl Sync for OwnedHandle {}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

struct Event(HANDLE);

impl Event {
    fn new() -> Result<Self, PipeError> {
        unsafe { CreateEventW(None, false, false, None) }
            .map(Self)
            .map_err(|error| PipeError::Win32("CreateEventW", error))
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn overlapped_read(
    handle: HANDLE,
    destination: &mut [u8],
    deadline: Instant,
    cancel: &AtomicBool,
) -> Result<usize, PipeError> {
    let event = Event::new()?;
    let mut overlapped = OVERLAPPED {
        hEvent: event.0,
        ..Default::default()
    };
    let mut immediate = 0u32;
    match unsafe {
        ReadFile(
            handle,
            Some(destination),
            Some(&mut immediate),
            Some(&mut overlapped),
        )
    } {
        Ok(()) => Ok(immediate as usize),
        Err(error) if error.code() == ERROR_IO_PENDING.to_hresult() => {
            wait_overlapped(handle, &overlapped, event.0, deadline, cancel)
        }
        Err(error) => Err(PipeError::Win32("ReadFile(pipe)", error)),
    }
}

fn overlapped_write(
    handle: HANDLE,
    source: &[u8],
    deadline: Instant,
    cancel: &AtomicBool,
) -> Result<usize, PipeError> {
    let event = Event::new()?;
    let mut overlapped = OVERLAPPED {
        hEvent: event.0,
        ..Default::default()
    };
    let mut immediate = 0u32;
    match unsafe {
        WriteFile(
            handle,
            Some(source),
            Some(&mut immediate),
            Some(&mut overlapped),
        )
    } {
        Ok(()) => Ok(immediate as usize),
        Err(error) if error.code() == ERROR_IO_PENDING.to_hresult() => {
            wait_overlapped(handle, &overlapped, event.0, deadline, cancel)
        }
        Err(error) => Err(PipeError::Win32("WriteFile(pipe)", error)),
    }
}

fn wait_overlapped(
    handle: HANDLE,
    overlapped: &OVERLAPPED,
    event: HANDLE,
    deadline: Instant,
    cancel: &AtomicBool,
) -> Result<usize, PipeError> {
    loop {
        if cancel.load(Ordering::Acquire) {
            cancel_overlapped(handle, overlapped);
            return Err(PipeError::Cancelled);
        }
        let now = Instant::now();
        if now >= deadline {
            cancel_overlapped(handle, overlapped);
            return Err(PipeError::Timeout);
        }
        let wait = deadline.saturating_duration_since(now).min(WAIT_SLICE);
        let milliseconds = u32::try_from(wait.as_millis().max(1)).unwrap_or(u32::MAX);
        let result = unsafe { WaitForSingleObject(event, milliseconds) };
        if result == WAIT_OBJECT_0 {
            let mut transferred = 0u32;
            unsafe { GetOverlappedResult(handle, overlapped, &mut transferred, false) }
                .map_err(|error| PipeError::Win32("GetOverlappedResult(pipe)", error))?;
            return Ok(transferred as usize);
        }
        if result != WAIT_TIMEOUT {
            cancel_overlapped(handle, overlapped);
            return Err(PipeError::Win32(
                "WaitForSingleObject(pipe)",
                windows::core::Error::from_win32(),
            ));
        }
    }
}

fn cancel_overlapped(handle: HANDLE, overlapped: &OVERLAPPED) {
    unsafe {
        let _ = CancelIoEx(handle, Some(overlapped));
        let mut ignored = 0u32;
        let _ = GetOverlappedResult(handle, overlapped, &mut ignored, true);
    }
}

struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

impl SecurityDescriptor {
    fn for_current_user() -> Result<Self, PipeError> {
        let sid = current_user_sid()?;
        let sddl = format!("D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;{sid})");
        let wide: Vec<u16> = sddl.encode_utf16().chain(Some(0)).collect();
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(wide.as_ptr()),
                SDDL_REVISION_1,
                &mut descriptor,
                None,
            )
        }
        .map_err(|error| PipeError::Win32("build private pipe DACL", error))?;
        Ok(Self(descriptor))
    }
}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        unsafe {
            let _ = LocalFree(HLOCAL(self.0 .0));
        }
    }
}

fn current_user_sid() -> Result<String, PipeError> {
    let mut token = HANDLE::default();
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }
        .map_err(|error| PipeError::Win32("OpenProcessToken", error))?;
    let token = OwnedHandle(token);
    let mut required = 0u32;
    let _ = unsafe { GetTokenInformation(token.0, TokenUser, None, 0, &mut required) };
    if required == 0 {
        return Err(PipeError::Win32(
            "size TokenUser",
            windows::core::Error::from_win32(),
        ));
    }
    let words = (required as usize)
        .checked_add(std::mem::size_of::<usize>() - 1)
        .ok_or(PipeError::InvalidFrame)?
        / std::mem::size_of::<usize>();
    let mut storage = vec![0usize; words];
    unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            Some(storage.as_mut_ptr().cast()),
            required,
            &mut required,
        )
    }
    .map_err(|error| PipeError::Win32("GetTokenInformation(TokenUser)", error))?;
    let user = unsafe { &*(storage.as_ptr().cast::<TOKEN_USER>()) };
    let mut text = PWSTR::null();
    unsafe { ConvertSidToStringSidW(user.User.Sid, &mut text) }
        .map_err(|error| PipeError::Win32("ConvertSidToStringSidW", error))?;
    if text.is_null() {
        return Err(PipeError::InvalidFrame);
    }
    let mut length = 0usize;
    unsafe {
        while *text.0.add(length) != 0 {
            length += 1;
        }
    }
    let value = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text.0, length) });
    unsafe {
        let _ = LocalFree(HLOCAL(text.0.cast()));
    }
    Ok(value)
}

pub(crate) fn never_cancelled() -> AtomicBool {
    AtomicBool::new(false)
}

pub(crate) fn connect_client(name: &str) -> Result<Pipe, PipeError> {
    let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            GENERIC_READ.0 | GENERIC_WRITE.0,
            FILE_SHARE_MODE(0),
            None,
            OPEN_EXISTING,
            FILE_FLAG_OVERLAPPED,
            None,
        )
    }
    .map_err(|error| PipeError::Win32("open private pipe", error))?;
    Ok(Pipe::from_handle(handle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferail_ntfs::{decode_frame, encode_frame, DuMessage};

    #[test]
    fn private_dacl_and_overlapped_round_trip_work_for_current_user() {
        let server = PipeServer::create().unwrap();
        let name = server.name.clone();
        let client = std::thread::spawn(move || {
            let pipe = connect_client(&name).unwrap();
            let frame = encode_frame(0, &DuMessage::Hello { helper_pid: 77 }).unwrap();
            pipe.write_frame(
                &frame,
                Instant::now() + Duration::from_secs(5),
                &never_cancelled(),
            )
            .unwrap();
        });
        server
            .connect(Instant::now() + Duration::from_secs(5), &never_cancelled())
            .unwrap();
        let frame = server
            .pipe
            .read_frame(Instant::now() + Duration::from_secs(5), &never_cancelled())
            .unwrap();
        assert_eq!(
            decode_frame(&frame, Some(0)).unwrap().1,
            DuMessage::Hello { helper_pid: 77 }
        );
        client.join().unwrap();
    }
}
