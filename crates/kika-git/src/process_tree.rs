//! A Windows Job Object around one git child. Killing `git.exe` does not
//! stop the processes it started (the wrapper's real git, hooks), so each
//! child is put in a job of its own and the whole job is stopped instead.
//! This is the Windows side of what a process group does on unix.

use std::io;
use std::os::windows::io::AsRawHandle;
use std::process::Child;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};

/// An owned, unnamed Job Object holding one child and everything it starts.
/// Closing it ends whatever is still inside.
pub(crate) struct ProcessTree(HANDLE);

// SAFETY: the handle is owned and never shared outside this type, and it is
// only passed to kernel calls that are safe to make from any thread.
unsafe impl Send for ProcessTree {}
// SAFETY: as above; `terminate` takes `&self` and the kernel serialises it.
unsafe impl Sync for ProcessTree {}

impl ProcessTree {
    /// A new job holding `child`. Its children join it as they start, so
    /// stopping the job stops the whole tree.
    pub(crate) fn for_child(child: &Child) -> io::Result<Self> {
        // SAFETY: both arguments may be null: no security attributes, no name.
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if job.is_null() {
            return Err(io::Error::last_os_error());
        }
        // Owned from here, so every early return below closes the handle.
        let tree = Self(job);
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: `limits` is a live, correctly sized value of the type the
        // information class names, and the kernel only reads it.
        let set = unsafe {
            SetInformationJobObject(
                tree.0,
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if set == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `child` is borrowed, so its process handle is open for
        // the whole call.
        if unsafe { AssignProcessToJobObject(tree.0, child.as_raw_handle()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(tree)
    }

    /// Stop every process in the tree. Idempotent; a tree whose processes
    /// have all exited has nothing to stop, so errors are ignored.
    pub(crate) fn terminate(&self) {
        // SAFETY: the handle is open until `drop`.
        let _ = unsafe { TerminateJobObject(self.0, 1) };
    }

    /// Whether `child` is in this tree.
    #[cfg(test)]
    pub(crate) fn contains(&self, child: &Child) -> io::Result<bool> {
        use windows_sys::Win32::System::JobObjects::IsProcessInJob;

        let mut inside = 0;
        // SAFETY: both handles are open for the call, and `inside` is a live
        // out-parameter.
        if unsafe { IsProcessInJob(child.as_raw_handle(), self.0, &mut inside) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(inside != 0)
    }
}

impl Drop for ProcessTree {
    /// Closing the last handle ends whatever is still in the job, because of
    /// `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`.
    fn drop(&mut self) {
        // SAFETY: the handle is owned, open, and never used again.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use crate::runner::{CliRunner, GitCommand, GitRunner, cli_child_tree_holds_git};

    #[test]
    fn a_spawned_git_is_in_its_tree_and_a_kill_ends_it() {
        let tmp = crate::fake::TempDir::new("tree");
        let dir = tmp.path();
        let big: String = (0..500_000).map(|i| format!("line {i}\n")).collect();
        std::fs::write(dir.join("big.txt"), big).unwrap();
        let runner = CliRunner::new(dir);
        let cmd = GitCommand::new("diff")
            .arg("--no-index")
            .arg("--")
            .arg("/dev/null")
            .arg("big.txt")
            .standard_env();

        let child = runner.spawn(&cmd).unwrap();
        // SAFETY: the handle comes from `CliRunner::spawn`, so it is a `CliChild`.
        assert_eq!(
            unsafe { cli_child_tree_holds_git(&child.handle) },
            Some(true)
        );

        child.handle.kill();
        let mut stdout = child.stdout;
        let (tx, rx) = mpsc::channel();
        let handle = child.handle;
        std::thread::spawn(move || {
            let _ = std::io::copy(&mut stdout, &mut std::io::sink());
            let _ = tx.send(handle.finish());
        });
        let output = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the killed tree kept git running")
            .unwrap();
        assert!(!output.success);
    }
}
