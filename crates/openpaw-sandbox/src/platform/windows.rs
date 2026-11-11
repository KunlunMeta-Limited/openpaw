//! 职责：复用Windows token/ACL/Job，提供普通进程托管和受限能力明确拒绝。
//! 输入输出：命令/工作区/取消到真实输出、退出码或错误。
//! 依赖：原Win32创建、Job、句柄和管道API。
//! 禁止职责：不将环境变量或防火墙尽力提示宣称隔离，不实现Office协议。
//! Windows sandbox implementation using Restricted Token + Capability SID + Job Object.
//!
//! Reference: OpenAI Codex windows-sandbox-rs approach
//! - CreateRestrictedToken with DISABLE_MAX_PRIVILEGE | LUA_TOKEN | WRITE_RESTRICTED
//! - Random Capability SID for file access control via ACL
//! - Job Object with JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
//! - 当前网络限制不可验证，受限策略在ACL/进程副作用前拒绝
//!
//! CreateRestrictedToken + Capability SID
//! ACL on writable paths via Capability SID
//! Job Object负责本地进程树清理；保留token/ACL原实现，不宣称已提供网络隔离

use std::mem;
use std::path::Path;
use std::ptr;
use std::time::Instant;

use windows::Win32::Foundation::*;
use windows::Win32::Security::Authorization::*;
use windows::Win32::Security::*;
use windows::Win32::Storage::FileSystem::*;
use windows::Win32::System::JobObjects::*;
use windows::Win32::System::Pipes::*;
use windows::Win32::System::Threading::*;

use crate::audit::AuditLog;
use crate::error::SandboxError;
use crate::policy::{FileSystemPolicy, SandboxPolicy};
use crate::SandboxResult;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Token restriction flags for CreateRestrictedToken.
const RESTRICTED_TOKEN_FLAGS: CREATE_RESTRICTED_TOKEN_FLAGS =
    CREATE_RESTRICTED_TOKEN_FLAGS(DISABLE_MAX_PRIVILEGE.0 | LUA_TOKEN.0 | WRITE_RESTRICTED.0);

/// Sub-authority value for our custom capability SID.
/// Uses SECURITY_NT_AUTHORITY (5) with sub-authority base 1.
const CAPABILITY_SID_SUBAUTH0: u32 = 1;

// ---------------------------------------------------------------------------
// RAII wrappers for Windows handles
// ---------------------------------------------------------------------------

/// RAII guard for a Windows HANDLE that closes it on drop.
struct OwnedHandle {
    handle: HANDLE,
}

impl OwnedHandle {
    fn new(handle: HANDLE) -> Self {
        Self { handle }
    }

    fn raw(&self) -> HANDLE {
        self.handle
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.handle.is_invalid() {
            // SAFETY: `self.handle` is a valid Windows HANDLE owned
            // exclusively by this `OwnedHandle`. `is_invalid()` guards the
            // NULL/-1 sentinels so we never pass an invalid handle to
            // CloseHandle. Drop runs exactly once, and the handle value is
            // never reused after closing — the guard is being destroyed.
            let _ = unsafe { CloseHandle(self.handle) };
        }
    }
}

/// RAII guard for a locally allocated SID.
struct OwnedSid {
    sid: PSID,
}

impl OwnedSid {
    fn new(sid: PSID) -> Self {
        Self { sid }
    }

    fn raw(&self) -> PSID {
        self.sid
    }
}

impl Drop for OwnedSid {
    fn drop(&mut self) {
        if !self.sid.is_invalid() {
            // SAFETY: `self.sid` is a valid PSID owned exclusively by this
            // `OwnedSid`; `is_invalid()` excludes the NULL sentinel. The pointer
            // is not aliased (no other reference exists) and is never used after
            // it is freed here. The cast to `HLOCAL` reinterprets the same
            // pointer value for the LocalFree allocation family.
            unsafe {
                LocalFree(Some(HLOCAL(self.sid.0 as *mut _)));
            }
        }
    }
}

/// RAII guard for ACL modifications. Removes the added ACE on drop.
struct AclGuard {
    paths: Vec<std::path::PathBuf>,
    sid: PSID,
}

impl AclGuard {
    fn new(paths: Vec<std::path::PathBuf>, sid: PSID) -> Self {
        Self { paths, sid }
    }
}

impl Drop for AclGuard {
    fn drop(&mut self) {
        for path in &self.paths {
            remove_write_acl(path, self.sid).ok();
        }
    }
}

/// RAII guard for a Job Object. Closing the handle kills all associated processes
/// (when JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE is set).
struct JobGuard {
    handle: Option<OwnedHandle>,
}

impl JobGuard {
    fn new(handle: OwnedHandle) -> Self {
        Self {
            handle: Some(handle),
        }
    }

    fn terminate_and_wait(&self) -> Result<(), SandboxError> {
        // SAFETY：本守卫拥有有效Job；无breakaway标记，子孙不能正常脱离Job。
        unsafe {
            TerminateJobObject(self.raw(), 1)
                .map_err(|e| SandboxError::JobObject(e.to_string()))?;
        }
        loop {
            let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { mem::zeroed() };
            unsafe {
                QueryInformationJobObject(
                    Some(self.raw()),
                    JobObjectBasicAccountingInformation,
                    (&mut info as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                    mem::size_of_val(&info) as u32,
                    None,
                )
                .map_err(|e| SandboxError::JobObject(e.to_string()))?;
            }
            if info.ActiveProcesses == 0 {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    fn raw(&self) -> HANDLE {
        self.handle.as_ref().map_or(HANDLE::default(), |h| h.raw())
    }
}

impl Drop for JobGuard {
    fn drop(&mut self) {
        let _ = self.terminate_and_wait();
        // Drop the handle, which closes it and kills child processes
        // (because KILL_ON_JOB_CLOSE is set)
        self.handle.take();
    }
}

// ---------------------------------------------------------------------------
// CreateRestrictedToken + Capability SID
// ---------------------------------------------------------------------------

/// Create a restricted token from the current process token.
///
/// Applies:
/// - `DISABLE_MAX_PRIVILEGE`: removes all privileges from the token
/// - `LUA_TOKEN`: creates a limited user account token
/// - `WRITE_RESTRICTED`: restricts write access to only paths where
///   the restricting SIDs have write ACEs
fn create_restricted_token(
    restricting_sids: &[SID_AND_ATTRIBUTES],
) -> Result<OwnedHandle, SandboxError> {
    let current_process = unsafe {
        // SAFETY: `GetCurrentProcess` takes no arguments and returns a
        // pseudo-handle to the current process; it cannot fail and involves no
        // pointers, so there is no aliasing or lifetime concern.
        GetCurrentProcess()
    };
    let mut process_token = HANDLE::default();

    // SAFETY: `current_process` is a valid pseudo-handle. `process_token`
    // is a stack-local `HANDLE` whose address is passed as the out-parameter; on
    // success the API writes a valid handle into it, which is then owned by
    // `_token_guard` (closed on drop). The access-mask flags are compile-time
    // constants.
    unsafe {
        OpenProcessToken(
            current_process,
            TOKEN_DUPLICATE | TOKEN_ADJUST_DEFAULT | TOKEN_QUERY,
            &mut process_token,
        )
        .map_err(|e| SandboxError::TokenCreation(format!("OpenProcessToken failed: {e}")))?;
    }

    let _token_guard = OwnedHandle::new(process_token);

    let mut restricted_token = HANDLE::default();

    // Prepare restricting SIDs slice
    let sid_count = restricting_sids.len() as u32;

    // SAFETY: `process_token` is a valid, owned handle kept alive by
    // `_token_guard` for the duration of this call. `restricting_sids` is a
    // borrowed slice whose `SID_AND_ATTRIBUTES` entries point at SIDs owned by
    // the caller (e.g. `cap_sid` in `exec_with_restricted_token`), all of which
    // outlive this call. `restricted_token` is a stack-local out-param; on
    // success the written handle is wrapped in `OwnedHandle` and returned, so it
    // cannot leak.
    unsafe {
        CreateRestrictedToken(
            process_token,
            RESTRICTED_TOKEN_FLAGS,
            None,
            None,
            if sid_count > 0 {
                Some(restricting_sids)
            } else {
                None
            },
            &mut restricted_token,
        )
        .map_err(|e| SandboxError::TokenCreation(format!("CreateRestrictedToken failed: {e}")))?;
    }

    Ok(OwnedHandle::new(restricted_token))
}

/// Generate a random Capability SID.
///
/// Uses SECURITY_NT_AUTHORITY (5) with sub-authorities [1, random].
/// The random component ensures each sandbox invocation has a unique SID
/// that can be used for ACL-based access control.
///
/// The random component is generated using a cryptographic RNG
/// (`getrandom`) instead of the previous `timestamp_nanos + pid` scheme,
/// which was predictable to an attacker who knew the approximate time
/// and process ID.
fn generate_capability_sid() -> Result<OwnedSid, SandboxError> {
    let mut sid: PSID = PSID::default();

    // Use a cryptographically secure random number generator
    // instead of timestamp+pid, which was predictable.
    let mut random_bytes = [0u8; 4];
    getrandom::getrandom(&mut random_bytes)
        .map_err(|e| SandboxError::SidGeneration(format!("getrandom failed: {e}")))?;
    let random_subauth = u32::from_le_bytes(random_bytes);

    let authority = SID_IDENTIFIER_AUTHORITY {
        Value: [0, 0, 0, 0, 0, 5], // SECURITY_NT_AUTHORITY
    };

    // SAFETY: `authority` is a valid stack-local reference. `sid` is a
    // zero-initialized `PSID` whose address is passed as the out-parameter; on
    // success the API allocates and writes a valid PSID into it, which is then
    // owned by `OwnedSid` (freed on drop). The sub-authority count (2) matches
    // the two meaningful sub-authority values supplied; the trailing zeros are
    // unused per the API contract.
    unsafe {
        AllocateAndInitializeSid(
            &authority,
            2,
            CAPABILITY_SID_SUBAUTH0,
            random_subauth,
            0,
            0,
            0,
            0,
            0,
            0,
            &mut sid,
        )
        .map_err(|e| {
            SandboxError::SidGeneration(format!("AllocateAndInitializeSid failed: {e}"))
        })?;
    }

    Ok(OwnedSid::new(sid))
}

// ---------------------------------------------------------------------------
// ACL on writable paths
// ---------------------------------------------------------------------------

/// Add a write ACE for the given SID on the specified path.
///
/// This grants the SID `FILE_GENERIC_WRITE` access on the path's DACL.
/// Combined with WRITE_RESTRICTED on the token, the sandboxed process
/// can only write to paths where this SID has write access.
fn add_write_acl(path: &Path, sid: PSID) -> Result<(), SandboxError> {
    let path_str = path.to_str().ok_or_else(|| SandboxError::AclOperation {
        path: path.display().to_string(),
        reason: "path is not valid UTF-8".to_string(),
    })?;

    let wide_path = to_wide(path_str);

    unsafe {
        // Get the current security descriptor
        let mut existing_sd = PSECURITY_DESCRIPTOR::default();
        let mut existing_dacl: *mut ACL = ptr::null_mut();

        let result = GetNamedSecurityInfoW(
            windows::core::PCWSTR(wide_path.as_ptr()),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(&mut existing_dacl),
            None,
            &mut existing_sd,
        );

        if result != ERROR_SUCCESS {
            return Err(SandboxError::AclOperation {
                path: path.display().to_string(),
                reason: format!("GetNamedSecurityInfo failed with error {:?}", result),
            });
        }

        let _sd_guard = SdGuard(existing_sd);

        // Build EXPLICIT_ACCESS for our SID with write access
        let ea = EXPLICIT_ACCESS_W {
            grfAccessPermissions: FILE_GENERIC_WRITE.0,
            grfAccessMode: GRANT_ACCESS,
            grfInheritance: SUB_CONTAINERS_AND_OBJECTS_INHERIT,
            Trustee: TRUSTEE_W {
                pMultipleTrustee: ptr::null_mut(),
                MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_UNKNOWN,
                ptstrName: windows::core::PWSTR(sid.0 as *mut _),
            },
        };

        // Merge with existing DACL
        let mut new_dacl: *mut ACL = ptr::null_mut();
        let set_result = SetEntriesInAclW(Some(&[ea]), Some(existing_dacl), &mut new_dacl);

        if set_result != ERROR_SUCCESS {
            return Err(SandboxError::AclOperation {
                path: path.display().to_string(),
                reason: format!("SetEntriesInAcl failed with error {:?}", set_result),
            });
        }

        let _dacl_guard = LocalAllocGuard(new_dacl as *mut _);

        // Set the new DACL on the path
        let set_result = SetNamedSecurityInfoW(
            windows::core::PWSTR(wide_path.as_ptr() as *mut _),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(new_dacl),
            None,
        );

        if set_result != ERROR_SUCCESS {
            return Err(SandboxError::AclOperation {
                path: path.display().to_string(),
                reason: format!("SetNamedSecurityInfo failed with error {:?}", set_result),
            });
        }

        Ok(())
    }
}

/// Remove the write ACE for the given SID from the specified path.
///
/// This is the cleanup operation that restores the original ACL.
fn remove_write_acl(path: &Path, sid: PSID) -> Result<(), SandboxError> {
    let path_str = path.to_str().ok_or_else(|| SandboxError::AclOperation {
        path: path.display().to_string(),
        reason: "path is not valid UTF-8".to_string(),
    })?;

    let wide_path = to_wide(path_str);

    unsafe {
        // Get current DACL
        let mut existing_sd = PSECURITY_DESCRIPTOR::default();
        let mut existing_dacl: *mut ACL = ptr::null_mut();

        let result = GetNamedSecurityInfoW(
            windows::core::PCWSTR(wide_path.as_ptr()),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(&mut existing_dacl),
            None,
            &mut existing_sd,
        );

        if result != ERROR_SUCCESS {
            return Err(SandboxError::AclOperation {
                path: path.display().to_string(),
                reason: format!("GetNamedSecurityInfo failed with error {:?}", result),
            });
        }

        let _sd_guard = SdGuard(existing_sd);

        // Build EXPLICIT_ACCESS to revoke our SID's write access
        let ea = EXPLICIT_ACCESS_W {
            grfAccessPermissions: FILE_GENERIC_WRITE.0,
            grfAccessMode: REVOKE_ACCESS,
            grfInheritance: SUB_CONTAINERS_AND_OBJECTS_INHERIT,
            Trustee: TRUSTEE_W {
                pMultipleTrustee: ptr::null_mut(),
                MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_UNKNOWN,
                ptstrName: windows::core::PWSTR(sid.0 as *mut _),
            },
        };

        let mut new_dacl: *mut ACL = ptr::null_mut();
        let set_result = SetEntriesInAclW(Some(&[ea]), Some(existing_dacl), &mut new_dacl);

        if set_result != ERROR_SUCCESS {
            return Err(SandboxError::AclOperation {
                path: path.display().to_string(),
                reason: format!(
                    "SetEntriesInAcl (revoke) failed with error {:?}",
                    set_result
                ),
            });
        }

        let _dacl_guard = LocalAllocGuard(new_dacl as *mut _);

        let set_result = SetNamedSecurityInfoW(
            windows::core::PWSTR(wide_path.as_ptr() as *mut _),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(new_dacl),
            None,
        );

        if set_result != ERROR_SUCCESS {
            return Err(SandboxError::AclOperation {
                path: path.display().to_string(),
                reason: format!(
                    "SetNamedSecurityInfo (revoke) failed with error {:?}",
                    set_result
                ),
            });
        }

        Ok(())
    }
}

/// Apply write ACLs on all writable paths for the given policy.
///
/// Returns an `AclGuard` that removes the ACEs on drop.
fn apply_writable_acls(
    policy: &SandboxPolicy,
    sid: PSID,
) -> Result<Option<AclGuard>, SandboxError> {
    let writable_paths = match policy.fs_policy() {
        FileSystemPolicy::ReadOnly | FileSystemPolicy::FullAccess => return Ok(None),
        FileSystemPolicy::WorkspaceWrite { roots } => roots,
    };

    let mut modified_paths = Vec::new();
    for root in writable_paths {
        // Ensure the path exists before modifying ACL
        if !root.exists() {
            std::fs::create_dir_all(&root).map_err(|e| SandboxError::AclOperation {
                path: root.display().to_string(),
                reason: format!("failed to create directory: {e}"),
            })?;
        }

        match add_write_acl(&root, sid) {
            Ok(()) => modified_paths.push(root.clone()),
            Err(e) => {
                // Roll back any already-applied ACLs
                for prev_path in &modified_paths {
                    remove_write_acl(prev_path, sid).ok();
                }
                return Err(e);
            }
        }
    }

    Ok(Some(AclGuard::new(modified_paths, sid)))
}

// ---------------------------------------------------------------------------
// Job Object
// ---------------------------------------------------------------------------

/// Create a Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`.
///
/// When the job object handle is closed, all processes in the job are terminated.
/// This ensures sandboxed processes are cleaned up even if the parent crashes.
fn create_job_object() -> Result<JobGuard, SandboxError> {
    // SAFETY: `CreateJobObjectW(None, None)` passes no name and no security
    // attributes, so there are no pointer/lifetime concerns. On success it returns
    // a fresh job handle that is immediately wrapped in `OwnedHandle`.
    let job = unsafe {
        CreateJobObjectW(None, None)
            .map_err(|e| SandboxError::JobObject(format!("CreateJobObject failed: {e}")))?
    };

    let job_handle = OwnedHandle::new(job);

    // Configure: kill all processes when the job handle is closed
    // SAFETY: `JOBOBJECT_EXTENDED_LIMIT_INFORMATION` is a plain-old-data
    // Win32 struct for which an all-zero bit pattern is a valid value; we then
    // overwrite only `LimitFlags` before use.
    let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { mem::zeroed() };
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

    // SAFETY: `job` is a valid, owned handle (kept alive by `job_handle`
    // for this call). `&info` is a valid, properly-sized stack reference whose
    // type matches the `JobObjectExtendedLimitInformation` info class and the
    // passed byte size.
    unsafe {
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const _,
            mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
        .map_err(|e| SandboxError::JobObject(format!("SetInformationJobObject failed: {e}")))?;
    }

    Ok(JobGuard::new(job_handle))
}

/// Assign a process to a Job Object.
fn assign_to_job_object(job: HANDLE, process_handle: HANDLE) -> Result<(), SandboxError> {
    // SAFETY: `job` (owned by the caller's `JobGuard`) and `process_handle`
    // (owned by the caller's `ProcessContext`) are valid handles that outlive this
    // call. The caller guarantees neither is the invalid sentinel. No pointers are
    // retained by the API beyond the call.
    unsafe {
        AssignProcessToJobObject(job, process_handle)
            .map_err(|e| SandboxError::JobObject(format!("AssignProcessToJobObject failed: {e}")))
    }
}

// ---------------------------------------------------------------------------
// Process creation with restricted token
// ---------------------------------------------------------------------------

// STARTUPINFOEX只继承本次管道，避免并发Run继承彼此写端而无法EOF。
struct AttributeList {
    initialized: bool,
    storage: Vec<usize>,
}
impl AttributeList {
    fn raw(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        LPPROC_THREAD_ATTRIBUTE_LIST(self.storage.as_mut_ptr().cast())
    }
    fn new(handles: &[HANDLE]) -> Result<Self, SandboxError> {
        let mut bytes = 0;
        // SAFETY：第一次空指针调用仅查询缓冲区大小；第二次提供usize对齐的稳定内存。
        unsafe {
            let _ = InitializeProcThreadAttributeList(None, 1, None, &mut bytes);
        }
        if bytes == 0 {
            return Err(SandboxError::ExecutionFailed(
                "attribute list size unavailable".into(),
            ));
        }
        let mut list = Self {
            initialized: false,
            storage: vec![0; bytes.div_ceil(mem::size_of::<usize>())],
        };
        unsafe {
            InitializeProcThreadAttributeList(Some(list.raw()), 1, None, &mut bytes)
                .map_err(|e| SandboxError::ExecutionFailed(e.to_string()))?;
            list.initialized = true;
            UpdateProcThreadAttribute(
                list.raw(),
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                Some(handles.as_ptr().cast()),
                mem::size_of_val(handles),
                None,
                None,
            )
            .map_err(|e| SandboxError::ExecutionFailed(e.to_string()))?;
        }
        Ok(list)
    }
}
impl Drop for AttributeList {
    fn drop(&mut self) {
        // SAFETY：成功初始化的list拥有稳定存储，删除不释放Vec本身。
        if self.initialized {
            unsafe {
                DeleteProcThreadAttributeList(self.raw());
            }
        }
    }
}
// cmd遇到UNC当前目录会静默改用Windows目录，不能在错误工作区执行已批准命令。
fn cmd_cwd(cwd: Option<&Path>) -> Result<Option<std::path::PathBuf>, SandboxError> {
    use std::path::{Component, Prefix};
    let Some(cwd) = cwd else { return Ok(None) };
    let reject_unc = |path: &Path| {
        matches!(path.components().next(), Some(Component::Prefix(prefix))
            if matches!(prefix.kind(), Prefix::UNC(..) | Prefix::VerbatimUNC(..) | Prefix::DeviceNS(..) | Prefix::Verbatim(..)))
    };
    if reject_unc(cwd) {
        return Err(SandboxError::ExecutionFailed(
            "cmd does not support UNC/device working directories; refusing directory fallback"
                .into(),
        ));
    }
    let canonical = std::fs::canonicalize(cwd)?;
    if reject_unc(&canonical) {
        return Err(SandboxError::ExecutionFailed(
            "cmd does not support UNC/device working directories; refusing directory fallback"
                .into(),
        ));
    }
    let mut components = canonical.components();
    if let Some(Component::Prefix(prefix)) = components.next() {
        if let Prefix::VerbatimDisk(drive) = prefix.kind() {
            let mut path = std::path::PathBuf::from(format!("{}:", char::from(drive)));
            path.extend(components);
            return Ok(Some(path));
        }
    }
    Ok(Some(canonical))
}
fn create_process_with_token(
    token: Option<HANDLE>,
    command: &str,
    cwd: Option<&Path>,
) -> Result<ProcessContext, SandboxError> {
    if command.contains('\0') {
        return Err(SandboxError::ExecutionFailed("command contains NUL".into()));
    }
    let cwd = cmd_cwd(cwd)?;
    let cwd = cwd.as_deref();
    let sa = SECURITY_ATTRIBUTES {
        nLength: mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: ptr::null_mut(),
        bInheritHandle: windows::core::BOOL(1),
    };
    fn pipe(sa: &SECURITY_ATTRIBUTES) -> Result<(OwnedHandle, OwnedHandle), SandboxError> {
        let mut read = HANDLE::default();
        let mut write = HANDLE::default();
        // SAFETY：两个输出句柄成功返回后立刻分别纳入RAII；后续失败不会泄漏。
        unsafe {
            CreatePipe(&mut read, &mut write, Some(sa), 0)
                .map_err(|e| SandboxError::ExecutionFailed(e.to_string()))?;
        }
        let read = OwnedHandle::new(read);
        let write = OwnedHandle::new(write);
        unsafe {
            SetHandleInformation(read.raw(), HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0))
                .map_err(|e| SandboxError::ExecutionFailed(e.to_string()))?;
        }
        Ok((read, write))
    }
    let (stdout_read, stdout_write) = pipe(&sa)?;
    let (stderr_read, stderr_write) = pipe(&sa)?;
    // SAFETY：NUL为系统空设备；继承句柄由本次handle list限定，成功立即拥有。
    let stdin = OwnedHandle::new(unsafe {
        CreateFileW(
            windows::core::w!("NUL"),
            GENERIC_READ.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            Some(&sa),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
        .map_err(|e| SandboxError::ExecutionFailed(e.to_string()))?
    });
    let inherited = [stdin.raw(), stdout_write.raw(), stderr_write.raw()];
    let mut attributes = AttributeList::new(&inherited)?;
    let mut startup: STARTUPINFOEXW = unsafe { mem::zeroed() };
    startup.StartupInfo.cb = mem::size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = stdin.raw();
    startup.StartupInfo.hStdOutput = stdout_write.raw();
    startup.StartupInfo.hStdError = stderr_write.raw();
    startup.lpAttributeList = attributes.raw();
    // 固定系统cmd，不依赖PATH/Git Bash；/D禁用AutoRun，/V:OFF禁用延迟展开。
    let shell = std::path::PathBuf::from(
        std::env::var_os("SystemRoot")
            .ok_or_else(|| SandboxError::ExecutionFailed("SystemRoot is unavailable".into()))?,
    )
    // cmd自身会解析命令行中的斜杠选项；路径必须使用本机分隔符，
    // 否则System32/cmd.exe中的/c可被误读为执行参数，损坏真实命令。
    .join("System32")
    .join("cmd.exe");
    use std::os::windows::ffi::OsStrExt;
    let application: Vec<u16> = shell.as_os_str().encode_wide().chain([0]).collect();
    let mut cmd_line = to_wide(&format!(
        "\"{}\" /D /V:OFF /S /C \"{command}\"",
        shell.display()
    ));
    let cwd_wide = cwd.map(|p| p.as_os_str().encode_wide().chain([0]).collect::<Vec<_>>());
    let mut env = crate::process::tool_environment();
    env.sort_by_key(|(k, _)| k.to_uppercase());
    let environment: Vec<u16> = env
        .iter()
        .flat_map(|(k, v)| {
            format!("{k}={v}")
                .encode_utf16()
                .chain([0])
                .collect::<Vec<_>>()
        })
        .chain([0])
        .collect();
    let cwd_ptr = cwd_wide
        .as_ref()
        .map_or(windows::core::PCWSTR::null(), |v| {
            windows::core::PCWSTR(v.as_ptr())
        });
    let flags = CREATE_NO_WINDOW
        | CREATE_SUSPENDED
        | CREATE_UNICODE_ENVIRONMENT
        | EXTENDED_STARTUPINFO_PRESENT;
    let mut info: PROCESS_INFORMATION = unsafe { mem::zeroed() };
    // SAFETY：所有UTF16/环境/属性/句柄存储均在系统调用期间有效；API可以改写cmd_line。
    let result = unsafe {
        match token {
            Some(token) => CreateProcessAsUserW(
                Some(token),
                windows::core::PCWSTR(application.as_ptr()),
                Some(windows::core::PWSTR(cmd_line.as_mut_ptr())),
                None,
                None,
                true,
                flags,
                Some(environment.as_ptr().cast()),
                cwd_ptr,
                &startup.StartupInfo,
                &mut info,
            ),
            None => CreateProcessW(
                windows::core::PCWSTR(application.as_ptr()),
                Some(windows::core::PWSTR(cmd_line.as_mut_ptr())),
                None,
                None,
                true,
                flags,
                Some(environment.as_ptr().cast()),
                cwd_ptr,
                &startup.StartupInfo,
                &mut info,
            ),
        }
    };
    result.map_err(|e| SandboxError::ExecutionFailed(format!("create suspended cmd: {e}")))?;
    Ok(ProcessContext {
        process_handle: OwnedHandle::new(info.hProcess),
        thread_handle: OwnedHandle::new(info.hThread),
        stdout_read,
        stderr_read,
    })
}
struct ProcessContext {
    process_handle: OwnedHandle,
    thread_handle: OwnedHandle,
    stdout_read: OwnedHandle,
    stderr_read: OwnedHandle,
}
impl Drop for ProcessContext {
    fn drop(&mut self) {
        // SAFETY：失败路径（含加入Job前）也必须终止挂起进程并等待leader，不能仅close句柄。
        unsafe {
            if WaitForSingleObject(self.process_handle.raw(), 0) != WAIT_OBJECT_0 {
                let _ = TerminateProcess(self.process_handle.raw(), 1);
                let _ = WaitForSingleObject(self.process_handle.raw(), INFINITE);
            }
        }
    }
}
struct PipeCapture {
    bytes: Vec<u8>,
    truncated: bool,
    eof: bool,
}
impl PipeCapture {
    fn new() -> Self {
        Self {
            bytes: vec![],
            truncated: false,
            eof: false,
        }
    }
    fn drain(&mut self, handle: HANDLE) -> Result<(), SandboxError> {
        if self.eof {
            return Ok(());
        }
        // 每次有界排空后处理另一管道及取消，防止连续写stdout饿死stderr。
        for _ in 0..16 {
            let mut available = 0;
            // SAFETY：本函数是该管道唯一reader；仅在Peek报告字节可用后同步ReadFile，不阻塞等进程结束。
            let peek = unsafe { PeekNamedPipe(handle, None, 0, None, Some(&mut available), None) };
            if let Err(e) = peek {
                if e.code() == ERROR_BROKEN_PIPE.to_hresult() {
                    self.eof = true;
                    break;
                }
                return Err(SandboxError::ExecutionFailed(format!("PeekNamedPipe: {e}")));
            }
            if available == 0 {
                break;
            }
            let mut chunk = [0u8; 8192];
            let take = (available as usize).min(chunk.len());
            let mut read = 0;
            unsafe {
                ReadFile(handle, Some(&mut chunk[..take]), Some(&mut read), None)
                    .map_err(|e| SandboxError::ExecutionFailed(format!("ReadFile: {e}")))?;
            }
            if read == 0 {
                self.eof = true;
                break;
            }
            let keep = (read as usize).min(crate::process::OUTPUT_LIMIT - self.bytes.len());
            self.bytes.extend_from_slice(&chunk[..keep]);
            self.truncated |= keep < read as usize;
        }
        Ok(())
    }
    fn text(self) -> String {
        let mut text = String::from_utf8_lossy(&self.bytes).into_owned();
        if self.truncated {
            text.push_str("\n[output truncated; remaining bytes drained]");
        }
        text
    }
}
impl ProcessContext {
    fn process_raw(&self) -> HANDLE {
        self.process_handle.raw()
    }
    fn resume_and_collect(
        &self,
        job: &JobGuard,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<(i32, String, String), SandboxError> {
        // SAFETY：主线程由本调用以挂起方式创建，句柄由self拥有。
        if unsafe { ResumeThread(self.thread_handle.raw()) } == u32::MAX {
            return Err(SandboxError::ExecutionFailed("ResumeThread failed".into()));
        }
        let mut stdout = PipeCapture::new();
        let mut stderr = PipeCapture::new();
        let mut cleanup = false;
        let mut cancelled = false;
        loop {
            stdout.drain(self.stdout_read.raw())?;
            stderr.drain(self.stderr_read.raw())?;
            let wait = unsafe { WaitForSingleObject(self.process_handle.raw(), 0) };
            if wait != WAIT_OBJECT_0 && wait != WAIT_TIMEOUT {
                return Err(SandboxError::ExecutionFailed(format!(
                    "wait process: {wait:?}"
                )));
            }
            if !cleanup && (wait == WAIT_OBJECT_0 || cancel.is_cancelled()) {
                cancelled = cancel.is_cancelled();
                job.terminate_and_wait()?;
                cleanup = true;
            }
            if cleanup && stdout.eof && stderr.eof {
                unsafe {
                    let wait = WaitForSingleObject(self.process_handle.raw(), INFINITE);
                    if wait != WAIT_OBJECT_0 {
                        return Err(SandboxError::ExecutionFailed("process wait failed".into()));
                    }
                }
                if cancelled || cancel.is_cancelled() {
                    return Err(SandboxError::ExecutionFailed(
                        "command cancelled; Job processes reaped".into(),
                    ));
                }
                let mut code = 0;
                unsafe {
                    GetExitCodeProcess(self.process_handle.raw(), &mut code)
                        .map_err(|e| SandboxError::ExecutionFailed(e.to_string()))?;
                }
                return Ok((code as i32, stdout.text(), stderr.text()));
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}

// ---------------------------------------------------------------------------
// Helper utilities
// ---------------------------------------------------------------------------

/// Convert a Rust string to a null-terminated wide string (UTF-16).
fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// RAII guard for LocalFree-allocated memory.
struct LocalAllocGuard(*mut core::ffi::c_void);

impl Drop for LocalAllocGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: `self.0` is a non-null pointer to memory allocated by
            // `LocalAlloc` (via `SetEntriesInAclW`) and owned exclusively by this
            // guard. The null check excludes the "no allocation" case. The pointer
            // is not used after it is freed here.
            unsafe {
                LocalFree(Some(HLOCAL(self.0 as *mut _)));
            }
        }
    }
}

/// RAII guard for security descriptors allocated by GetNamedSecurityInfo.
struct SdGuard(PSECURITY_DESCRIPTOR);

impl Drop for SdGuard {
    fn drop(&mut self) {
        if !self.0 .0.is_null() {
            // SAFETY: `self.0` is a `PSECURITY_DESCRIPTOR` allocated by
            // `GetNamedSecurityInfoW` and owned exclusively by this guard. The
            // inner-null check excludes the "no allocation" case. The descriptor
            // is freed via `LocalFree` per the API contract and is not used after.
            unsafe {
                LocalFree(Some(HLOCAL(self.0 .0 as *mut _)));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Main entry point
// ---------------------------------------------------------------------------

/// 功能：在原token后端执行受限命令，不允许普通执行兜底
/// 参数：policy/command/cwd为输入，cancel为token
/// 返回：清理后的结果
/// 错误：当前无可验证网络隔离，受限组合明确拒绝
/// 状态：拒绝发生在创建进程或修改ACL前
/// 并发与取消：被支持后端必须使用同一Job/管道回收路径
/// 权限：token或环境提示不能证明NoAccess/AllowDomains已落实
/// 关联：需求/设计 DES-OP-005；REQ-OP-006。
/// Stability: unstable.
pub(crate) async fn exec_restricted_cancellable(
    policy: &SandboxPolicy,
    command: &str,
    cwd: Option<&Path>,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<SandboxResult, SandboxError> {
    ensure_supported_policy(policy)?;
    let policy = policy.clone();
    let command = command.to_owned();
    let cwd = cwd.map(Path::to_path_buf);
    tokio::task::spawn_blocking(move || {
        let start = Instant::now();
        let name = format!("{policy:?}");
        let (exit_code, stdout, stderr) =
            exec_with_restricted_token(&policy, &command, cwd.as_deref(), &cancel)?;
        let mut audit = AuditLog::new(&command, cwd.as_deref(), &name, "restricted_token");
        audit.exit_code = exit_code;
        audit.duration_ms = start.elapsed().as_millis() as u64;
        Ok(SandboxResult {
            stdout,
            stderr,
            exit_code,
            audit,
        })
    })
    .await
    .map_err(|e| SandboxError::ExecutionFailed(format!("managed Windows task: {e}")))?
}
fn ensure_supported_policy(policy: &SandboxPolicy) -> Result<(), SandboxError> {
    if policy.is_restricted() {
        return Err(SandboxError::PlatformUnavailable("Windows token/ACL backend cannot enforce requested network isolation; refusing execution".into()));
    }
    Ok(())
}
fn exec_with_restricted_token(
    policy: &SandboxPolicy,
    command: &str,
    cwd: Option<&Path>,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<(i32, String, String), SandboxError> {
    if cancel.is_cancelled() {
        return Err(SandboxError::ExecutionFailed(
            "command cancelled before spawn".into(),
        ));
    }
    let cap_sid = generate_capability_sid()?;
    let token = create_restricted_token(&[SID_AND_ATTRIBUTES {
        Sid: cap_sid.raw(),
        Attributes: 0,
    }])?;
    let _acls = apply_writable_acls(policy, cap_sid.raw())?;
    let job = create_job_object()?;
    let process = create_process_with_token(Some(token.raw()), command, cwd)?;
    assign_to_job_object(job.raw(), process.process_raw())?;
    process.resume_and_collect(&job, cancel)
}

/// 功能：使用原挂起进程/Job实现普通用户本地执行
/// 参数：command/cwd为输入，cancel为调用token
/// 返回：清理完成的输出与审计
/// 错误：创建/Job分配/回收或取消错误
/// 状态：普通用户CreateProcessW，不申请受限token权限
/// 并发与取消：工作线程轮询token并杀Job等待真实退出，不只spawn_blocking
/// 权限：明确普通执行，不声称文件或网络隔离
/// 关联：需求/设计 DES-OP-005；REQ-OP-006。
/// Stability: unstable.
pub(crate) async fn exec_local(
    command: &str,
    cwd: Option<&Path>,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<SandboxResult, SandboxError> {
    let command = command.to_owned();
    let cwd = cwd.map(Path::to_path_buf);
    tokio::task::spawn_blocking(move || {
        if cancel.is_cancelled() {
            return Err(SandboxError::ExecutionFailed(
                "command cancelled before spawn".into(),
            ));
        }
        let start = Instant::now();
        let job = create_job_object()?;
        let process = create_process_with_token(None, &command, cwd.as_deref())?;
        assign_to_job_object(job.raw(), process.process_raw())?;
        let (exit_code, stdout, stderr) = process.resume_and_collect(&job, &cancel)?;
        let mut audit = AuditLog::new(&command, cwd.as_deref(), "None", "job_object");
        audit.exit_code = exit_code;
        audit.duration_ms = start.elapsed().as_millis() as u64;
        Ok(SandboxResult {
            stdout,
            stderr,
            exit_code,
            audit,
        })
    })
    .await
    .map_err(|e| SandboxError::ExecutionFailed(format!("managed Windows task: {e}")))?
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_sid_generation() {
        let sid = generate_capability_sid();
        assert!(sid.is_ok(), "Should be able to generate a Capability SID");
        let owned = sid.unwrap();
        assert!(!owned.raw().is_invalid(), "SID should be valid");
    }

    #[test]
    fn capability_sid_unique() {
        let sid1 = generate_capability_sid().unwrap();
        // Small delay to ensure different random draws
        std::thread::sleep(std::time::Duration::from_millis(1));
        let sid2 = generate_capability_sid().unwrap();

        unsafe {
            let equal = EqualSid(sid1.raw(), sid2.raw()).is_ok();
            // SIDs should be different (different random component)
            assert!(!equal, "Each SID should be unique");
        }
    }

    #[test]
    fn job_object_creation() {
        let job = create_job_object();
        assert!(job.is_ok(), "Should be able to create a Job Object");
    }

    #[test]
    fn restricted_token_creation() {
        let token = create_restricted_token(&[]);
        match token {
            Ok(_) => {}
            Err(SandboxError::TokenCreation(msg)) => {
                // May fail in non-elevated contexts
                eprintln!(
                    "Restricted token creation failed (expected in some environments): {msg}"
                );
            }
            Err(e) => panic!("Unexpected error: {e}"),
        }
    }

    #[test]
    fn restricted_token_with_sid() {
        let cap_sid = generate_capability_sid().unwrap();
        let sid_and_attr = SID_AND_ATTRIBUTES {
            Sid: cap_sid.raw(),
            Attributes: 0,
        };
        let token = create_restricted_token(&[sid_and_attr]);
        match token {
            Ok(_) => {}
            Err(SandboxError::TokenCreation(msg)) => {
                eprintln!(
                    "Restricted token with SID failed (expected in some environments): {msg}"
                );
            }
            Err(e) => panic!("Unexpected error: {e}"),
        }
    }

    #[test]
    fn to_wide_conversion() {
        let wide = to_wide("hello");
        assert_eq!(wide.last(), Some(&0));
        assert_eq!(wide.len(), 6); // "hello" = 5 chars + null
    }

    #[test]
    fn add_write_acl_on_temp_dir() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let sid = generate_capability_sid().unwrap();

        let result = add_write_acl(path, sid.raw());
        // Should succeed on a real path
        assert!(
            result.is_ok(),
            "ACL addition should succeed: {:?}",
            result.err()
        );

        // Clean up
        remove_write_acl(path, sid.raw()).ok();
    }
    #[test]
    fn failed_job_assignment_reaps_the_suspended_process() {
        let dir = tempfile::tempdir().unwrap();
        let process =
            create_process_with_token(None, "echo invalid > should-not-run", Some(dir.path()))
                .unwrap();
        let mut duplicate = HANDLE::default();
        // SAFETY：观察用副本延长句柄而非进程生命周期，ctx析构后检查真实结束。
        unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                process.process_raw(),
                GetCurrentProcess(),
                &mut duplicate,
                0,
                false,
                DUPLICATE_SAME_ACCESS,
            )
            .unwrap();
        }
        let duplicate = OwnedHandle::new(duplicate);
        assert!(assign_to_job_object(HANDLE::default(), process.process_raw()).is_err());
        drop(process);
        assert_eq!(
            unsafe { WaitForSingleObject(duplicate.raw(), 0) },
            WAIT_OBJECT_0
        );
        assert!(!dir.path().join("should-not-run").exists());
    }
}
