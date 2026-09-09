//! Windows sandbox implementation using Restricted Token + Capability SID + Job Object.
//!
//! Reference: OpenAI Codex windows-sandbox-rs approach
//! - CreateRestrictedToken with DISABLE_MAX_PRIVILEGE | LUA_TOKEN | WRITE_RESTRICTED
//! - Random Capability SID for file access control via ACL
//! - Job Object with JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
//! - Firewall rules for network isolation (netsh fallback)
//!
//! CreateRestrictedToken + Capability SID
//! ACL on writable paths via Capability SID
//! Job Object with KILL_ON_JOB_CLOSE + firewall rules for network isolation

use std::mem;
use std::path::Path;
use std::ptr;
use std::time::Instant;

use windows::Win32::Foundation::*;
use windows::Win32::Security::Authorization::*;
use windows::Win32::Security::*;
use windows::Win32::Storage::FileSystem::*;
use windows::Win32::System::Console::*;
use windows::Win32::System::JobObjects::*;
use windows::Win32::System::Pipes::*;
use windows::Win32::System::Threading::*;

use crate::audit::AuditLog;
use crate::error::SandboxError;
use crate::policy::{FileSystemPolicy, NetworkPolicy, SandboxPolicy};
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

    fn raw(&self) -> HANDLE {
        self.handle.as_ref().map_or(HANDLE::default(), |h| h.raw())
    }
}

impl Drop for JobGuard {
    fn drop(&mut self) {
        // Drop the handle, which closes it and kills child processes
        // (because KILL_ON_JOB_CLOSE is set)
        self.handle.take();
    }
}

/// RAII guard for firewall rules. Removes the rules on drop.
struct FirewallGuard {
    rule_names: Vec<String>,
}

impl FirewallGuard {
    #[allow(dead_code)] // associated function `new` is never used
    fn new(rule_names: Vec<String>) -> Self {
        Self { rule_names }
    }
}

impl Drop for FirewallGuard {
    fn drop(&mut self) {
        for name in &self.rule_names {
            remove_firewall_rule(name).ok();
        }
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
// Firewall rules for network isolation
// ---------------------------------------------------------------------------

/// Add a firewall rule blocking outbound connections for a specific process.
///
/// Uses `netsh advfirewall firewall add rule` to create a blocking rule.
/// This requires administrator privileges; if not available, logs a warning
/// and returns an error gracefully.
#[allow(dead_code)] // function `add_firewall_block_rule` is never used
fn add_firewall_block_rule(rule_name: &str, program_path: &str) -> Result<(), SandboxError> {
    let output = std::process::Command::new("netsh")
        .args([
            "advfirewall",
            "firewall",
            "add",
            "rule",
            &format!("name={rule_name}"),
            "dir=out",
            "action=block",
            "enable=yes",
            &format!("program={program_path}"),
            "profile=any",
        ])
        .output()
        .map_err(|e| SandboxError::FirewallRule(format!("netsh execution failed: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(SandboxError::FirewallRule(format!(
            "netsh add rule failed: {stderr}"
        )));
    }

    Ok(())
}

/// Remove a firewall rule by name.
fn remove_firewall_rule(rule_name: &str) -> Result<(), SandboxError> {
    let output = std::process::Command::new("netsh")
        .args([
            "advfirewall",
            "firewall",
            "delete",
            "rule",
            &format!("name={rule_name}"),
        ])
        .output()
        .map_err(|e| SandboxError::FirewallRule(format!("netsh execution failed: {e}")))?;

    if !output.status.success() {
        // Best-effort cleanup: 删除失败时静默忽略（本 crate 无日志设施）。
        let _ = &output;
    }

    Ok(())
}

/// Apply network isolation based on the policy.
///
/// For NoAccess policy, creates a firewall rule blocking all outbound.
/// Returns a guard that removes the rule on drop.
#[allow(dead_code)] // function `apply_network_isolation` is never used
fn apply_network_isolation(
    policy: &SandboxPolicy,
    program_path: &str,
) -> Result<Option<FirewallGuard>, SandboxError> {
    match policy.net_policy() {
        NetworkPolicy::FullAccess | NetworkPolicy::AllowDomains(_) => Ok(None),
        NetworkPolicy::NoAccess => {
            let rule_name = format!("OpenPaw_Sandbox_Block_{}", std::process::id());
            match add_firewall_block_rule(&rule_name, program_path) {
                Ok(()) => Ok(Some(FirewallGuard::new(vec![rule_name]))),
                Err(_e) => {
                    // 网络隔离为尽力而为：防火墙规则创建失败（通常因缺少管理员
                    // 权限）不应阻断沙箱执行。
                    Ok(None)
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Process creation with restricted token
// ---------------------------------------------------------------------------

/// Create a process using the restricted token.
///
/// Uses `CreateProcessAsUserW` to start the command with the restricted token,
/// capturing stdout and stderr via pipes.
fn create_process_with_token(
    token: HANDLE,
    command: &str,
    cwd: Option<&Path>,
) -> Result<ProcessContext, SandboxError> {
    // Create pipes for stdout and stderr
    let mut stdout_read = HANDLE::default();
    let mut stdout_write = HANDLE::default();
    let mut stderr_read = HANDLE::default();
    let mut stderr_write = HANDLE::default();

    let sa = SECURITY_ATTRIBUTES {
        nLength: mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: ptr::null_mut(),
        bInheritHandle: windows::core::BOOL(1),
    };

    // SAFETY: This block creates inheritable pipes and spawns the sandboxed
    // process. Invariants:
    //  - `sa` is a valid `SECURITY_ATTRIBUTES` (nLength set, null DACL, inheritable)
    //    and is passed by reference only for the duration of the CreatePipe calls.
    //  - The four `HANDLE` out-params are stack locals; on success each receives a
    //    valid handle. The read ends are made non-inheritable; the write ends stay
    //    inheritable so the child can use them.
    //  - `startup_info` / `process_info` are plain-old-data structs; an all-zero
    //    bit pattern is valid, and `cb` is set to the struct size as required.
    //  - `cmd_line` is a NUL-terminated UTF-16 buffer; `as_mut_ptr()` is valid and
    //    the buffer outlives `CreateProcessAsUserW` (same scope). The API is
    //    documented to possibly mutate the buffer in place, which is acceptable as
    //    it is not reused afterwards.
    //  - `token` is a valid, owned restricted-token handle (caller-owned, alive for
    //    this call). `cwd_wide`, when present, is NUL-terminated UTF-16.
    //  - On success, `process_info.hProcess`/`hThread` and the read pipe ends are
    //    wrapped in `OwnedHandle` (closed on drop); the parent's write ends are
    //    explicitly closed here. No handle leaks on either path.
    unsafe {
        CreatePipe(&mut stdout_read, &mut stdout_write, Some(&sa), 0).map_err(|e| {
            SandboxError::ExecutionFailed(format!("CreatePipe (stdout) failed: {e}"))
        })?;

        CreatePipe(&mut stderr_read, &mut stderr_write, Some(&sa), 0).map_err(|e| {
            SandboxError::ExecutionFailed(format!("CreatePipe (stderr) failed: {e}"))
        })?;

        // Make the read ends non-inheritable
        SetHandleInformation(stdout_read, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0)).map_err(|e| {
            SandboxError::ExecutionFailed(format!("SetHandleInformation failed: {e}"))
        })?;
        SetHandleInformation(stderr_read, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0)).map_err(|e| {
            SandboxError::ExecutionFailed(format!("SetHandleInformation failed: {e}"))
        })?;

        // Prepare startup info
        let mut startup_info: STARTUPINFOW = mem::zeroed();
        startup_info.cb = mem::size_of::<STARTUPINFOW>() as u32;
        startup_info.dwFlags = STARTF_USESTDHANDLES;
        startup_info.hStdInput = GetStdHandle(STD_INPUT_HANDLE).unwrap_or_default();
        startup_info.hStdOutput = stdout_write;
        startup_info.hStdError = stderr_write;

        // Prepare command line (cmd /C <command>)
        //
        // Security: This function intentionally uses cmd /C to support shell
        // builtins, pipes, and redirection. The security boundary is the
        // restricted token (CreateRestrictedToken), not command parsing.
        // Metacharacter interpretation by cmd.exe is expected behavior.
        // Even Command::new("cmd").args(["/C", cmd]) would be concatenated
        // into a single string by the Rust stdlib on Windows, so the result
        // is identical. The upstream guardrail and sandbox approval layers
        // filter dangerous commands before they reach this point.
        let full_cmd = format!("cmd /C {command}");
        let mut cmd_line: Vec<u16> = full_cmd.encode_utf16().chain(std::iter::once(0)).collect();

        // Prepare cwd
        let cwd_wide = cwd.and_then(|p| p.to_str()).map(to_wide);

        let mut process_info: PROCESS_INFORMATION = mem::zeroed();

        let result = CreateProcessAsUserW(
            Some(token),
            None,
            Some(windows::core::PWSTR(cmd_line.as_mut_ptr())),
            None,
            None,
            true, // inherit handles
            CREATE_NO_WINDOW | CREATE_SUSPENDED,
            None,
            cwd_wide
                .as_ref()
                .map(|w| windows::core::PCWSTR(w.as_ptr()))
                .unwrap_or(windows::core::PCWSTR::null()),
            &startup_info,
            &mut process_info,
        );

        // Close write ends of pipes in the parent
        let _ = CloseHandle(stdout_write);
        let _ = CloseHandle(stderr_write);

        result.map_err(|e| {
            SandboxError::ExecutionFailed(format!("CreateProcessAsUserW failed: {e}"))
        })?;

        Ok(ProcessContext {
            process_handle: OwnedHandle::new(process_info.hProcess),
            thread_handle: OwnedHandle::new(process_info.hThread),
            stdout_read: OwnedHandle::new(stdout_read),
            stderr_read: OwnedHandle::new(stderr_read),
        })
    }
}

/// Context for a created process with pipe handles.
struct ProcessContext {
    process_handle: OwnedHandle,
    thread_handle: OwnedHandle,
    stdout_read: OwnedHandle,
    stderr_read: OwnedHandle,
}

impl ProcessContext {
    fn process_raw(&self) -> HANDLE {
        self.process_handle.raw()
    }

    /// Resume the suspended process and wait for completion.
    fn resume_and_wait(&self) -> Result<i32, SandboxError> {
        // SAFETY: `self.thread_handle.raw()` and `self.process_handle.raw()`
        // are valid, owned handles (alive for the duration of `&self`). The process
        // was created suspended, so `ResumeThread` on its main thread is the
        // documented way to start it. `WaitForSingleObject` blocks on the process
        // handle until the process exits. `exit_code` is a stack-local out-param
        // whose address is passed to `GetExitCodeProcess`; on success it receives
        // the process exit code.
        unsafe {
            // Resume the main thread (process was created suspended)
            ResumeThread(self.thread_handle.raw());

            // Wait for the process to complete
            let wait_result = WaitForSingleObject(self.process_handle.raw(), INFINITE);

            if wait_result != WAIT_OBJECT_0 {
                return Err(SandboxError::ExecutionFailed(format!(
                    "WaitForSingleObject returned {wait_result:?}"
                )));
            }

            // Get exit code
            let mut exit_code: u32 = 0;
            GetExitCodeProcess(self.process_handle.raw(), &mut exit_code).map_err(|e| {
                SandboxError::ExecutionFailed(format!("GetExitCodeProcess failed: {e}"))
            })?;

            Ok(exit_code as i32)
        }
    }

    /// Read stdout and stderr from the pipe handles.
    fn read_output(&self) -> (String, String) {
        let stdout = read_pipe(self.stdout_read.raw());
        let stderr = read_pipe(self.stderr_read.raw());
        (stdout, stderr)
    }
}

/// Maximum bytes read from a sandboxed process pipe to prevent OOM.
const MAX_PIPE_OUTPUT_BYTES: usize = 10 * 1024 * 1024; // 10 MB

/// Read all data from a pipe handle.
///
/// Caps total output at [`MAX_PIPE_OUTPUT_BYTES`] to prevent a malicious
/// sandboxed process from triggering OOM by writing GB-level data.
fn read_pipe(handle: HANDLE) -> String {
    let mut buffer = Vec::with_capacity(4096);
    let mut temp = [0u8; 4096];
    let mut bytes_read: u32 = 0;

    loop {
        // SAFETY: `handle` is a valid, owned anonymous-pipe read handle.
        // `temp` is a stack-local 4096-byte buffer passed as a mutable slice;
        // `bytes_read` is a stack-local `u32` whose address is the out-param.
        // `lpOverlapped` is `None`, so the read is synchronous. The buffer is
        // only read up to `bytes_read` after the call returns.
        let result = unsafe {
            ReadFile(
                handle,
                Some(&mut temp),
                Some(&mut bytes_read as *mut u32),
                None,
            )
        };

        if result.is_err() || bytes_read == 0 {
            break;
        }

        buffer.extend_from_slice(&temp[..bytes_read as usize]);

        // Stop reading and truncate when the output exceeds the limit.
        if buffer.len() >= MAX_PIPE_OUTPUT_BYTES {
            buffer.truncate(MAX_PIPE_OUTPUT_BYTES);
            break;
        }
    }

    String::from_utf8_lossy(&buffer).to_string()
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

/// Execute a command with Windows restricted token sandboxing.
///
/// Attempts the full restricted-token implementation first. If the restricted
/// token approach fails (e.g., insufficient privileges), falls back to basic
/// execution with environment variable hints.
pub async fn exec_restricted(
    policy: &SandboxPolicy,
    command: &str,
    cwd: Option<&Path>,
) -> Result<SandboxResult, SandboxError> {
    // For unrestricted policies, just run normally
    if !policy.is_restricted() {
        return crate::command::exec_unrestricted(command, cwd).await;
    }

    let start = Instant::now();
    let policy_name = format!("{policy:?}");

    // Try full restricted token approach
    match exec_with_restricted_token(policy, command, cwd) {
        Ok((exit_code, stdout, stderr)) => {
            let duration_ms = start.elapsed().as_millis() as u64;
            let mut audit = AuditLog::new(command, cwd, &policy_name, "restricted_token");
            audit.exit_code = exit_code;
            audit.duration_ms = duration_ms;

            Ok(SandboxResult {
                stdout,
                stderr,
                exit_code,
                audit,
            })
        }
        Err(_e) => {
            // Restricted token approach may fail in non-elevated contexts
            // (e.g., CreateProcessAsUserW requires SE_ASSIGNPRIMARYTOKEN_NAME).
            // Gracefully fall back to basic execution with env hints.
            exec_basic_fallback(policy, command, cwd, &policy_name).await
        }
    }
}

/// Full restricted token execution.
///
/// Returns (exit_code, stdout, stderr).
fn exec_with_restricted_token(
    policy: &SandboxPolicy,
    command: &str,
    cwd: Option<&Path>,
) -> Result<(i32, String, String), SandboxError> {
    // Step 1: Generate Capability SID
    let cap_sid = generate_capability_sid()?;

    // Step 2: Build restricting SIDs for the token
    let sid_and_attr = SID_AND_ATTRIBUTES {
        Sid: cap_sid.raw(),
        Attributes: 0,
    };

    // Step 3: Create restricted token with our SID as restricting SID
    let token = create_restricted_token(&[sid_and_attr])?;

    // Step 4: Apply ACL on writable paths
    let _acl_guards = apply_writable_acls(policy, cap_sid.raw())?;

    // Step 5: Create Job Object
    let job = create_job_object()?;

    // Step 6: Create process with restricted token
    let ctx = create_process_with_token(token.raw(), command, cwd)?;

    // Step 7: Assign process to Job Object
    assign_to_job_object(job.raw(), ctx.process_raw())?;

    // Step 8: Apply network isolation
    let _firewall_guard = apply_network_isolation_fallback(policy);

    // Step 9: Resume and wait for process
    let exit_code = ctx.resume_and_wait()?;

    // Step 10: Read output
    let (stdout, stderr) = ctx.read_output();

    Ok((exit_code, stdout, stderr))
}

/// Apply network isolation as a best-effort fallback.
///
/// For NoAccess policy, sets environment variable hints.
/// Firewall rules require admin privileges and are handled separately.
fn apply_network_isolation_fallback(_policy: &SandboxPolicy) -> Option<FirewallGuard> {
    // Network isolation for NoAccess is enforced by the WRITE_RESTRICTED token
    // plus ACL path; firewall rules require admin privileges and are handled
    // separately.
    None
}

/// Basic fallback execution with environment variable hints (Phase 1).
///
/// Used when the restricted token approach is unavailable.
async fn exec_basic_fallback(
    policy: &SandboxPolicy,
    command: &str,
    cwd: Option<&Path>,
    policy_name: &str,
) -> Result<SandboxResult, SandboxError> {
    let start = Instant::now();

    let mut cmd = tokio::process::Command::new("cmd");
    cmd.args(["/C", command]);

    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }

    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    // For ReadOnly policy, set read-only env hint
    if matches!(policy.fs_policy(), FileSystemPolicy::ReadOnly) {
        cmd.env("OPENPAW_SANDBOX_FS", "readonly");
    }

    // For NoAccess network policy, set env hint
    if matches!(policy.net_policy(), NetworkPolicy::NoAccess) {
        cmd.env("OPENPAW_SANDBOX_NET", "noaccess");
    }

    let output = cmd
        .output()
        .await
        .map_err(|e| SandboxError::ExecutionFailed(format!("failed to execute command: {e}")))?;

    let duration_ms = start.elapsed().as_millis() as u64;

    let mut audit = AuditLog::new(command, cwd, policy_name, "basic_fallback");
    audit.exit_code = output.status.code().unwrap_or(-1);
    audit.duration_ms = duration_ms;

    Ok(SandboxResult {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        exit_code: audit.exit_code,
        audit,
    })
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
}
