//! Handle to a running client, kept open so its exit code can be read after
//! it exits.

use std::fmt;
use std::sync::Arc;

use windows::Win32::Foundation::{CloseHandle, HANDLE, STILL_ACTIVE};
use windows::Win32::System::Threading::{
    GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
};

struct Handle(HANDLE);

// SAFETY: a process handle is a kernel object reference, usable from any thread.
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle came from OpenProcess and is closed once.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

#[derive(Clone)]
pub struct ProcessHandle(Arc<Handle>);

impl ProcessHandle {
    pub fn open(pid: u32) -> Option<Self> {
        // SAFETY: plain FFI call. Failure returns an error, not an invalid handle.
        let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE, false, pid) }.ok()?;
        Some(Self(Arc::new(Handle(h))))
    }

    /// Exit code, or `None` while the process is still running.
    pub fn exit_code(&self) -> Option<u32> {
        let mut code = 0u32;
        // SAFETY: valid handle with query rights, and `code` outlives the call.
        unsafe { GetExitCodeProcess(self.0.0, &mut code) }.ok()?;
        (code != STILL_ACTIVE.0 as u32).then_some(code)
    }
}

impl fmt::Debug for ProcessHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ProcessHandle")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exit_code_of(cmd: &str) -> Option<u32> {
        let mut child = std::process::Command::new("cmd").args(["/c", cmd]).spawn().unwrap();
        let h = ProcessHandle::open(child.id()).unwrap();
        child.wait().unwrap();
        h.exit_code()
    }

    #[test]
    fn reads_exit_code() {
        assert_eq!(exit_code_of("exit 0"), Some(0));
        assert_eq!(exit_code_of("exit 3"), Some(3));
    }
}
