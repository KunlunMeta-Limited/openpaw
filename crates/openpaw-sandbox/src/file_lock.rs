//! 职责：提供现有跨进程共享/独占建议锁及作用域释放。
//! 输入输出：锁文件路径转换为持锁守卫，冲突或系统错误返回io::Error。
//! 依赖：标准文件、Unix flock或Windows LockFileEx/UnlockFileEx。
//! 禁止职责：不承担Host调度、工作区授权、轮询等待或另一套锁存储。
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
//! explicitly unlocks before closing its handle on drop. Closing alone can
//! leave a Unix lock alive in a descriptor inherited by a concurrent spawn.
//! Panics unwind through the same release path; abrupt process termination
//! still relies on the operating system to close all inherited descriptors.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

/// 用途：在作用域内持有OS级建议锁，析构时先显式解锁再关闭文件。
/// 字段：私有_file拥有唯一由本守卫管理的锁文件句柄。
/// 来源去向：由try_lock_exclusive或try_lock_shared产生，退出作用域释放。
/// 约束：守卫不可克隆，不暴露文件；Unix继承的描述符不应延长守卫所有权。
/// 稳定性：保留已有公开类型与构造函数；内部释放细节可按平台修正。
/// 关联：兼容基线REQ-OP-002、共享资源设计DES-OP-006及缺陷D16。
pub struct FileLockGuard {
    _file: File,
}

/// 功能：创建或打开锁文件，非阻塞取得独占建议锁。
/// 参数：path为调用方选择且已授权的锁文件路径。
/// 返回：持锁守卫，析构时显式释放OS锁并关闭句柄。
/// 错误：路径、权限、锁冲突或平台系统调用失败。
/// 状态：可能创建锁文件，文件保留不代表仍被锁定。
/// 并发与取消：不同进程/句柄的共享与独占均被阻止；不等待，无异步取消点。
/// 权限：不扩大路径权限；建议锁仅约束遵守同一协议的参与方。
/// 关联：兼容基线REQ-OP-002、共享资源设计DES-OP-006及缺陷D16。
pub fn try_lock_exclusive(path: &Path) -> io::Result<FileLockGuard> {
    let file = open_lock_file(path)?;
    lock_inner(&file, /* exclusive */ true)?;
    Ok(FileLockGuard { _file: file })
}

/// 功能：创建或打开锁文件，非阻塞取得共享建议锁。
/// 参数：path为调用方选择且已授权的锁文件路径。
/// 返回：共享持锁守卫，析构只释放本次共享所有权。
/// 错误：路径、权限、独占锁冲突或平台系统调用失败。
/// 状态：可能创建锁文件；共享锁必须全部释放才能取得独占锁。
/// 并发与取消：允许独立共享持有者并存；不等待，无异步取消点。
/// 权限：不扩大路径权限；不阻止未遵守建议锁的外部写入。
/// 关联：兼容基线REQ-OP-002、共享资源设计DES-OP-006及缺陷D16。
pub fn try_lock_shared(path: &Path) -> io::Result<FileLockGuard> {
    let file = open_lock_file(path)?;
    lock_inner(&file, /* exclusive */ false)?;
    Ok(FileLockGuard { _file: file })
}

impl Drop for FileLockGuard {
    fn drop(&mut self) {
        // D16：flock属于open file description，另一线程fork出的描述符即使
        // 设置CLOEXEC也会在exec前持有它，close本句柄不能同步结束守卫所有权。
        // Windows也显式UnlockFileEx，避免依赖关闭句柄后的系统清理时序。
        // Drop不能上报错误；仍保留File随字段析构关闭的OS兜底，不panic。
        let _ = unlock_inner(&self._file);
    }
}

#[cfg(unix)]
fn unlock_inner(file: &File) -> io::Result<()> {
    use std::os::unix::io::AsRawFd;
    loop {
        // SAFETY: file在整个调用中持有有效描述符；LOCK_UN不改变文件内容。
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) } == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

#[cfg(windows)]
fn unlock_inner(file: &File) -> io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::UnlockFileEx;
    use windows::Win32::System::IO::OVERLAPPED;

    // SAFETY: file仍拥有有效句柄；零偏移与整文件长度和LockFileEx完全相同，
    // OVERLAPPED只在同步系统调用期间借用，不越过其生命周期。
    let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
    unsafe {
        UnlockFileEx(
            HANDLE(file.as_raw_handle()),
            None,
            u32::MAX,
            u32::MAX,
            &mut overlapped,
        )
        .map_err(|error| io::Error::other(error.to_string()))
    }
}

#[cfg(not(any(unix, windows)))]
fn unlock_inner(_file: &File) -> io::Result<()> {
    Ok(()) // 此平台的lock_inner从不成功，故不会构造持锁守卫。
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
        // 不使用sleep或重试掩盖释放竞态；失败时保留具体OS诊断。
        let again = try_lock_exclusive(&lock_path);
        assert!(
            again.is_ok(),
            "exclusive lock should succeed after release: {:?}",
            again.err()
        );
    }

    #[test]
    fn shared_lock_allows_concurrent_shared() {
        let dir = tempfile::tempdir().unwrap();
        let lock_path = dir.path().join("shared.lock");

        let _g1 = try_lock_shared(&lock_path).expect("first shared lock should succeed");
        let _g2 = try_lock_shared(&lock_path).expect("second shared lock should succeed");
        // Two shared locks can coexist; dropping both releases the file.
    }

    #[cfg(unix)]
    #[test]
    fn drop_releases_lock_while_inherited_description_is_alive() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inherited.lock");
        let guard = try_lock_exclusive(&path).unwrap();
        // try_clone与fork持有相同open file description；不启动子进程即可
        // 确定性复现另一测试线程fork到exec之间继承锁描述符的窗口。
        let inherited = guard._file.try_clone().unwrap();
        drop(guard);
        let next = try_lock_exclusive(&path)
            .expect("guard drop must release the lock before an inherited descriptor closes");
        drop(inherited);
        assert!(try_lock_exclusive(&path).is_err());
        drop(next);
        assert!(try_lock_exclusive(&path).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn shared_guard_drop_releases_its_inherited_description() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inherited-shared.lock");
        let guard = try_lock_shared(&path).unwrap();
        let inherited = guard._file.try_clone().unwrap();
        drop(guard);
        let next = try_lock_exclusive(&path)
            .expect("shared guard drop must release its inherited description");
        drop(inherited);
        assert!(try_lock_shared(&path).is_err());
        drop(next);
    }

    // 子测试仅在父测试传入独立临时路径时执行探测；普通cargo test不会
    // 依赖全局环境修改，也不递归派生进程。两个平台复用同一真实子进程入口。
    #[test]
    fn child_lock_probe() {
        let Some(path) = std::env::var_os("OPENPAW_TEST_FILE_LOCK_PATH") else {
            return;
        };
        let shared = std::env::var("OPENPAW_TEST_FILE_LOCK_SHARED").unwrap() == "true";
        let expected = std::env::var("OPENPAW_TEST_FILE_LOCK_EXPECTED").unwrap() == "true";
        let result = if shared {
            try_lock_shared(Path::new(&path))
        } else {
            try_lock_exclusive(Path::new(&path))
        };
        assert_eq!(
            result.is_ok(),
            expected,
            "cross-process lock ownership mismatch"
        );
    }

    fn probe_child(path: &Path, shared: bool, expected: bool) {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "file_lock::tests::child_lock_probe",
                "--nocapture",
            ])
            .env("OPENPAW_TEST_FILE_LOCK_PATH", path)
            .env("OPENPAW_TEST_FILE_LOCK_SHARED", shared.to_string())
            .env("OPENPAW_TEST_FILE_LOCK_EXPECTED", expected.to_string())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "child probe failed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn independent_processes_observe_shared_and_exclusive_ownership() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("跨进程 locks.lock");
        let exclusive = try_lock_exclusive(&path).unwrap();
        probe_child(&path, false, false);
        probe_child(&path, true, false);
        drop(exclusive);
        probe_child(&path, false, true);
        let first = try_lock_shared(&path).unwrap();
        let second = try_lock_shared(&path).unwrap();
        probe_child(&path, true, true);
        probe_child(&path, false, false);
        drop(first);
        probe_child(&path, false, false);
        drop(second);
        probe_child(&path, false, true);
    }
}
