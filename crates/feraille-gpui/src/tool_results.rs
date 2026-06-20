//! Shared placement context for tab-local tool result surfaces.
//!
//! The shell owns *where* a tool is hosted; each tool owns how its body reacts
//! to that placement. Keep this module small and UI-agnostic so tools can
//! depend on it without depending on the shell implementation.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolHostContext {
    Docked,
    Windowed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolHostEvent {
    HostChanged(ToolHostContext),
}
