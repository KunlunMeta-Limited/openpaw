//! OS-level advisory file locks.
//!
//! Historically the workspace guarded critical sections either by atomically
//! creating a lock file (`OpenOptions::create_new`) or via in-memory
//! `HashMap`/`Mutex` bookkeeping. Neither is robust across *processes*: the
//! `create_new` trick cannot be shared/read-locked and leaves stale files
//! behind on panic, and the in-memory map is per-process only.
//!
//! This module provides real OS-level advisory locks as a foundation that the
//! existing ad-hoc locks can migrate to incrementally. The public surface
//! mirrors `fs2::FileExt` (`try_lock_exclusive` / `try_lock_shared`) so the
//! eventual migration to the `fs2` crate — the intended unified dependency — is
//! a near-mechanical substitution.
//!
//! ## Why not `fs2` today?
//! This workspace builds **offline** from `vendor/` (see `.cargo/config.toml`),
//! and `fs2` is not vendored (it pulls `windows-sys 0.52`, also absent). The
//! implementation below therefore uses already-vendored crates (`windows` on
//! Windows, `libc` on Unix) to provide the same behaviour. When `fs2` is
//! vendored, replace the body of [`lock_inner`] with `file.try_lock_exclusive()`
//! / `file.try_lock_shared()` and delete the platform `cfg` branches.
//!
//! ## Migration path for existing locks
//! Replace ad-hoc `create_new` lock files with `try_lock_exclusive(path)`,
//! holding the returned [`FileLockGuard`] for the critical section. The guard
//! releases the lock on drop (the underlying handle/descriptor is closed), so
//! panics no longer leave stale lock files behind.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

/// A RAII guard that holds an OS-level advisory lock on a file.
///
/// The lock is released automatically when the guard is dropped (the owned
/// `File` is closed, which releases the advisory lock on both Windows and
/// Unix). There is no need — and no public way — to unlock early: scope the
/// guard to the critical section.
pub struct FileLockGuard {
    _file: File,
}

/// Try to acquire an *exclusive* (write) advisory lock on `path`, creating the
/// file if it does not yet exist. Returns `Err` (not a panic) if the lock is
/// already held by another process.
pub fn try_lock_exclusive(path: &Path) -> io::Result<FileLockGuard> {
    let file = open_lock_file(path)?;
    lock_inner(&file, /* exclusive */ true)?;
    Ok(FileLockGuard { _file: file })
}

/// Try to acquire a *shared* (read) advisory lock on `path`, creating the file
/// if it does not yet exist. Returns `Err` if the lock is held exclusively by
/// another process.
pub fn try_lock_shared(path: &Path) -> io::Result<FileLockGuard> {
    let file = open_lock_file(path)?;
    lock_inner(&file, /* exclusive */ false)?;
    Ok(FileLockGuard { _file: file })
}

/// Open (creating if necessary) the lock file in read+write mode.
fn open_lock_file(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
}

#[cfg(unix)]
fn lock_inner(file: &File, exclusive: bool) -> io::Result<()> {
    use std::os::unix::io::AsRawFd;
    let fd = file.as_raw_fd();
    let op = if exclusive {
        libc::LOCK_EX | libc::LOCK_NB
    } else {
        libc::LOCK_SH | libc::LOCK_NB
    };
    // SAFETY: `fd` is a valid open file descriptor backed by `file`, which is
    // kept alive by the caller (and ultimately by the `FileLockGuard`). `LOCK_NB`
    // makes the call non-blocking: on contention it returns -1 with `EWOULDBLOCK`
    // instead of blocking, which we surface as an `io::Error` to the caller.
    let rc = unsafe { libc::flock(fd, op) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(windows)]
fn lock_inner(file: &File, exclusive: bool) -> io::Result<()> {
    use std::mem;
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        LockFileEx, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY,
    };
    use windows::Win32::System::IO::OVERLAPPED;

    let handle = HANDLE(file.as_raw_handle());
    let mut flags = LOCKFILE_FAIL_IMMEDIATELY;
    if exclusive {
        flags |= LOCKFILE_EXCLUSIVE_LOCK;
    }

    // SAFETY: `handle` is a valid, owned file handle borrowed from `file` (kept
    // alive for the lock's lifetime by the `FileLockGuard`). `OVERLAPPED` is
    // zero-initialised so `Offset`/`OffsetHigh` are 0 — the lock begins at byte
    // 0. The length (low = u32::MAX, high = u32::MAX) covers the whole file,
    // matching `fs2` semantics. `LOCKFILE_FAIL_IMMEDIATELY` makes the call
    // non-blocking so contention is reported as an error instead of blocking.
    let mut overlapped: OVERLAPPED = unsafe { mem::zeroed() };
    unsafe {
        LockFileEx(handle, flags, None, u32::MAX, u32::MAX, &mut overlapped)
            .map_err(|e| io::Error::other(e.to_string()))?;
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn lock_inner(_file: &File, _exclusive: bool) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "OS-level file locks are not implemented on this platform",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exclusive_lock_blocks_second_acquire() {
        let dir = tempfile::tempdir().unwrap();
        let lock_path = dir.path().join("test.lock");

        let guard = try_lock_exclusive(&lock_path).expect("first exclusive lock should succeed");
        assert!(lock_path.exists(), "lock file should be created");

        // A second exclusive acquire on a *different* handle to the same file
        // should fail (non-blocking) while the first guard is held.
        let second = try_lock_exclusive(&lock_path);
        assert!(
            second.is_err(),
            "second exclusive lock should be blocked while the first is held"
        );

        drop(guard);
        // After releasing, a new exclusive lock should succeed again.
        let again = try_lock_exclusive(&lock_path);
        assert!(again.is_ok(), "exclusive lock should succeed after release");
    }

    #[test]
    fn shared_lock_allows_concurrent_shared() {
        let dir = tempfile::tempdir().unwrap();
        let lock_path = dir.path().join("shared.lock");

        let _g1 = try_lock_shared(&lock_path).expect("first shared lock should succeed");
        let _g2 = try_lock_shared(&lock_path).expect("second shared lock should succeed");
        // Two shared locks can coexist; dropping both releases the file.
    }
}
