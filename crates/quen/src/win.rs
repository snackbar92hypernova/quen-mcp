//! The Win32 pieces: the lock file, the Job Object launch, the detached daemon start, the named pipe
//! between proxy and daemon with its peer check, the startup mutex, the console Ctrl handler, the token's randomness,
//! the Documents and Pictures folders, registry reads, version resources and Authenticode, and the journal's file
//! operations (a rename that never replaces, file ids, the Recycle Bin), and the display kept awake during a session.
//! The capture module has its own, in the `windows` crate.

use std::collections::BTreeMap;
use std::env;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::iter::repeat_n;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::{OnceLock, mpsc};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_BROKEN_PIPE, ERROR_CANCELLED, ERROR_IO_PENDING,
    ERROR_MORE_DATA, ERROR_NO_DATA, ERROR_OPERATION_ABORTED, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED,
    ERROR_PIPE_NOT_CONNECTED, ERROR_SHARING_VIOLATION, FILETIME, GENERIC_READ, GENERIC_WRITE,
    GetLastError, HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, LocalFree,
    SetHandleInformation, WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::Cryptography::{
    BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom,
};
use windows_sys::Win32::Security::WinTrust::{
    WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA, WINTRUST_DATA_0, WINTRUST_FILE_INFO,
    WTD_CACHE_ONLY_URL_RETRIEVAL, WTD_CHOICE_FILE, WTD_REVOKE_NONE, WTD_STATEACTION_CLOSE,
    WTD_STATEACTION_VERIFY, WTD_UI_NONE, WinVerifyTrust,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OPEN_REPARSE_POINT, FILE_FLAG_OVERLAPPED,
    FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, GetDriveTypeW,
    GetFileInformationByHandle, GetFileVersionInfoSizeW, GetFileVersionInfoW,
    GetVolumeNameForVolumeMountPointW, GetVolumePathNameW, MOVEFILE_REPLACE_EXISTING,
    MOVEFILE_WRITE_THROUGH, MoveFileExW, OPEN_EXISTING, PIPE_ACCESS_DUPLEX, ReadFile,
    SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT, VS_FIXEDFILEINFO, VerQueryValueW, WriteFile,
};
use windows_sys::Win32::Storage::Packaging::Appx::GetCurrentPackageFullName;
use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicProcessIdList,
    JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
    TerminateJobObject,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, GetNamedPipeServerProcessId,
    PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES,
    PIPE_WAIT, WaitNamedPipeW,
};
use windows_sys::Win32::System::Power::{
    ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED, SetThreadExecutionState,
};
use windows_sys::Win32::System::Registry::{
    HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegCloseKey,
    RegEnumKeyExW, RegGetValueW, RegOpenKeyExW,
};
use windows_sys::Win32::System::SystemInformation::GetTickCount64;
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateEventW, CreateMutexW,
    CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess,
    GetExitCodeProcess, GetProcessTimes, INFINITE, InitializeProcThreadAttributeList, OpenProcess,
    OpenProcessToken, PROC_THREAD_ATTRIBUTE_DESKTOP_APP_POLICY, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROCESS_INFORMATION, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, QueryFullProcessImageNameW, ReleaseMutex, ResumeThread,
    STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute,
    WaitForSingleObject,
};
use windows_sys::Win32::UI::Shell::{
    FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FOF_WANTNUKEWARNING,
    FOLDERID_Documents, FOLDERID_Pictures, SHFILEOPSTRUCTW, SHFileOperationW, SHGetKnownFolderPath,
};
use windows_sys::core::BOOL;
use windows_sys::core::GUID;

fn check(ok: BOOL) -> io::Result<()> {
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Takes `path` exclusively (share mode 0: no other process can open it at all) and writes the holder into it.
/// `None` when another process holds it. The OS releases it when the file closes or quen dies. A reader (a status call,
/// an antivirus scan, the indexer) holds it for milliseconds, so a sharing violation is retried a few times first.
pub fn try_lock(path: &Path, holder: &serde_json::Value) -> io::Result<Option<File>> {
    let mut tries = 0;
    let mut file = loop {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(0)
            .open(path);
        match file {
            Ok(file) => break file,
            Err(e) if e.raw_os_error() == Some(ERROR_SHARING_VIOLATION as i32) => {
                tries += 1;
                if tries > 5 {
                    return Ok(None);
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(e),
        }
    };
    file.set_len(0)?;
    file.write_all(holder.to_string().as_bytes())?;
    file.sync_all()?;
    Ok(Some(file))
}

/// A Job Object that kills every process in it when its last handle closes, so the game dies with quen. Its
/// handle is not inheritable, so no child can keep it open.
pub struct Job(HANDLE);

// SAFETY: a job handle may be used from any thread; the handle is closed only in Drop.
unsafe impl Send for Job {}
unsafe impl Sync for Job {}

impl Job {
    pub fn new() -> io::Result<Job> {
        // SAFETY: null attributes give a non-inheritable, unnamed job; the limits outlive the call.
        unsafe {
            let handle = CreateJobObjectW(null(), null());
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            let job = Job(handle);
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            check(SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                size_of_val(&limits) as u32,
            ))?;
            Ok(job)
        }
    }

    /// Starts `argv` suspended, assigns it to the job, checks that it is in, then resumes it, so it never runs a
    /// single instruction outside the job (nor does anything it starts). Its environment is [`CHILD_ENV`] plus `env`.
    /// Its stdin is NUL, stdout and stderr go to `log`, and it inherits no other handle.
    pub fn spawn(
        &self,
        argv: &[String],
        env: &[(&str, &str)],
        cwd: &Path,
        log: &File,
    ) -> io::Result<Child> {
        let pi = create(
            argv,
            env,
            cwd,
            log,
            CREATE_SUSPENDED | CREATE_NO_WINDOW,
            false,
        )?;
        // SAFETY: the handles CreateProcessW returned; the thread handle is closed once, the process one by Child.
        unsafe {
            let child = Child {
                process: pi.hProcess,
            };
            let mut inside = 0;
            let placed = check(AssignProcessToJobObject(self.0, pi.hProcess))
                .and_then(|()| check(IsProcessInJob(pi.hProcess, self.0, &mut inside)))
                .and_then(|()| match inside {
                    0 => Err(io::Error::other(
                        "the process is not in the job after assignment",
                    )),
                    _ if ResumeThread(pi.hThread) == u32::MAX => Err(io::Error::last_os_error()),
                    _ => Ok(()),
                });
            CloseHandle(pi.hThread);
            if let Err(e) = placed {
                TerminateProcess(pi.hProcess, 1);
                return Err(e);
            }
            Ok(child)
        }
    }

    /// Kills every process in the job: the only way quen kills (never by image name).
    pub fn kill(&self) -> io::Result<()> {
        // SAFETY: a valid job handle.
        check(unsafe { TerminateJobObject(self.0, 1) })
    }

    /// The ids of the processes in the job now.
    pub fn pids(&self) -> io::Result<Vec<u32>> {
        // JOBOBJECT_BASIC_PROCESS_ID_LIST with room for 256 ids. ponytail: more than 256 processes is an error.
        #[repr(C)]
        struct List {
            assigned: u32,
            listed: u32,
            ids: [usize; 256],
        }
        let mut list = List {
            assigned: 0,
            listed: 0,
            ids: [0; 256],
        };
        // SAFETY: the buffer is the documented layout and its size is passed.
        check(unsafe {
            QueryInformationJobObject(
                self.0,
                JobObjectBasicProcessIdList,
                (&raw mut list).cast(),
                size_of::<List>() as u32,
                null_mut(),
            )
        })?;
        Ok(list.ids[..list.listed as usize]
            .iter()
            .map(|&id| id as u32)
            .collect())
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: closed once; with KILL_ON_JOB_CLOSE this kills whatever is still in the job.
        unsafe { CloseHandle(self.0) };
    }
}

/// `PROCESS_CREATION_DESKTOP_APP_BREAKAWAY_DISABLE_PROCESS_TREE` (in WindowsProgramming, a feature quen doesn't use).
static DESKTOP_APP_INSIDE: u32 = 2;

/// CreateProcessW for `argv` with `flags`, its environment [`CHILD_ENV`] plus `env` (`QUEN_*` only for `quen`, quen's
/// own daemon): stdin NUL, stdout and stderr `out`, and no other handle inherited. The caller owns the returned process
/// and thread handles.
fn create(
    argv: &[String],
    env: &[(&str, &str)],
    cwd: &Path,
    out: &File,
    flags: u32,
    quen: bool,
) -> io::Result<PROCESS_INFORMATION> {
    let mut cmd = command_line(argv)?;
    let env = env_block(env, quen)?;
    let cwd = wide(cwd.as_os_str())?;
    // Fresh handles, inheritable, and named in the handle list: the child gets exactly these two.
    let nul = File::open("NUL")?;
    let out = out.try_clone()?;
    let handles = [nul.as_raw_handle(), out.as_raw_handle()];
    // SAFETY: every pointer passed lives until CreateProcessW returns; the attribute list is deleted after it.
    unsafe {
        for handle in handles {
            check(SetHandleInformation(
                handle,
                HANDLE_FLAG_INHERIT,
                HANDLE_FLAG_INHERIT,
            ))?;
        }
        let mut size = 0;
        InitializeProcThreadAttributeList(null_mut(), 2, 0, &mut size);
        let mut list = vec![0usize; size.div_ceil(size_of::<usize>())];
        let list = list.as_mut_ptr().cast();
        check(InitializeProcThreadAttributeList(list, 2, 0, &mut size))?;
        let mut info: STARTUPINFOEXW = std::mem::zeroed();
        info.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        info.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        info.StartupInfo.hStdInput = handles[0];
        info.StartupInfo.hStdOutput = handles[1];
        info.StartupInfo.hStdError = handles[1];
        info.lpAttributeList = list;
        let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
        let created = check(UpdateProcThreadAttribute(
            list,
            0,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
            handles.as_ptr().cast(),
            size_of_val(&handles),
            null_mut(),
            null(),
        ))
        .and_then(|()| {
            // A packaged app's children (Store Python's) otherwise start outside its container, and so outside the
            // job, unkillable by it: they stay inside, all the way down.
            check(UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_DESKTOP_APP_POLICY as usize,
                (&raw const DESKTOP_APP_INSIDE).cast(),
                size_of::<u32>(),
                null_mut(),
                null(),
            ))
        })
        .and_then(|()| {
            check(CreateProcessW(
                null(),
                cmd.as_mut_ptr(),
                null(),
                null(),
                1,
                flags | CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT,
                env.as_ptr().cast(),
                cwd.as_ptr(),
                &info.StartupInfo,
                &mut pi,
            ))
        });
        DeleteProcThreadAttributeList(list);
        created?;
        Ok(pi)
    }
}

/// Starts `argv` with `flags` outside any quen job (the daemon and its short-lived starter), with the same
/// handle rules as [`Job::spawn`]. Fails, with the OS's reason, when `flags` asks to break away and the job refuses.
pub fn spawn_detached(
    argv: &[String],
    env: &[(&str, &str)],
    cwd: &Path,
    out: &File,
    flags: u32,
) -> io::Result<Child> {
    let pi = create(argv, env, cwd, out, flags, true)?;
    // SAFETY: the thread handle CreateProcessW returned, closed once.
    unsafe { CloseHandle(pi.hThread) };
    Ok(Child {
        process: pi.hProcess,
    })
}

/// Whether this process's own job (the innermost, if it is in one) kills it when the job's last handle closes, as the
/// MCP client's job does.
pub fn job_kills_on_close() -> io::Result<bool> {
    let mut inside = 0;
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: the pseudo handle of this process; a null job means this process's innermost job; the buffer is the
    // documented struct and its size.
    unsafe {
        check(IsProcessInJob(GetCurrentProcess(), null_mut(), &mut inside))?;
        if inside == 0 {
            return Ok(false);
        }
        check(QueryInformationJobObject(
            null_mut(),
            JobObjectExtendedLimitInformation,
            (&raw mut limits).cast(),
            size_of_val(&limits) as u32,
            null_mut(),
        ))?;
    }
    Ok(limits.BasicLimitInformation.LimitFlags & JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE != 0)
}

/// One end of a named pipe (proxy ↔ daemon), opened for overlapped I/O, so one thread can read while another
/// writes (synchronous I/O on one pipe handle serialises them, and a pending read would block every write).
pub struct Pipe(OwnedHandle);

impl Pipe {
    /// A new server instance of `name` (`\\.\pipe\...`): local clients only, byte mode, and only this user may open it.
    /// `first` refuses (access denied) when the name already exists, so a second daemon, or a squatter, is found.
    pub fn create(name: &str, first: bool) -> io::Result<Pipe> {
        let sddl = format!(
            "D:P(A;;GA;;;{})",
            process_user(unsafe { GetCurrentProcess() })?
        );
        let mut sd: PSECURITY_DESCRIPTOR = null_mut();
        // SAFETY: valid strings; the descriptor is freed after CreateNamedPipeW, which copies it.
        unsafe {
            check(ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide(sddl.as_ref())?.as_ptr(),
                SDDL_REVISION_1,
                &mut sd,
                null_mut(),
            ))?;
            let attrs = SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: sd,
                bInheritHandle: 0,
            };
            let first = if first {
                FILE_FLAG_FIRST_PIPE_INSTANCE
            } else {
                0
            };
            let handle = CreateNamedPipeW(
                wide(name.as_ref())?.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | first,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                64 * 1024,
                64 * 1024,
                0,
                &attrs,
            );
            let err = io::Error::last_os_error();
            LocalFree(sd);
            if handle == INVALID_HANDLE_VALUE {
                return Err(err);
            }
            Ok(Pipe(OwnedHandle::from_raw_handle(handle)))
        }
    }

    /// The client end of `name`, waiting while every instance is busy. NotFound when nothing listens on it. The server
    /// may only identify this process, never act as it.
    pub fn open(name: &str) -> io::Result<Pipe> {
        let wname = wide(name.as_ref())?;
        loop {
            // SAFETY: a valid name; the handle is owned at once.
            let handle = unsafe {
                CreateFileW(
                    wname.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    0,
                    null(),
                    OPEN_EXISTING,
                    FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                    null_mut(),
                )
            };
            if handle != INVALID_HANDLE_VALUE {
                // SAFETY: a fresh handle nothing else owns.
                return Ok(Pipe(unsafe { OwnedHandle::from_raw_handle(handle) }));
            }
            let err = io::Error::last_os_error();
            // SAFETY: a valid name.
            if err.raw_os_error() != Some(ERROR_PIPE_BUSY as i32)
                || unsafe { WaitNamedPipeW(wname.as_ptr(), 2000) } == 0
            {
                return Err(err);
            }
        }
    }

    /// A second handle to the same pipe: one for reading, one for writing.
    pub fn try_clone(&self) -> io::Result<Pipe> {
        Ok(Pipe(self.0.try_clone()?))
    }

    /// The process at the other end, for the server or the client side.
    pub fn peer_pid(&self, server: bool) -> io::Result<u32> {
        let mut pid = 0;
        let h = self.0.as_raw_handle();
        // SAFETY: a valid pipe handle.
        check(unsafe {
            if server {
                GetNamedPipeClientProcessId(h, &mut pid)
            } else {
                GetNamedPipeServerProcessId(h, &mut pid)
            }
        })?;
        Ok(pid)
    }

    /// Waits for a client on this server instance, asking `give_up` about once a second. A client that connects just as
    /// it gives up still counts. Whether one came.
    pub fn accept(&self, mut give_up: impl FnMut() -> bool) -> io::Result<bool> {
        let h = self.0.as_raw_handle();
        // SAFETY: the OVERLAPPED and its event live until the operation is complete (GetOverlappedResult waits).
        unsafe {
            let mut ov: OVERLAPPED = std::mem::zeroed();
            ov.hEvent = CreateEventW(null(), 1, 0, null());
            if ov.hEvent.is_null() {
                return Err(io::Error::last_os_error());
            }
            let event = OwnedHandle::from_raw_handle(ov.hEvent);
            if ConnectNamedPipe(h, &mut ov) == 0 {
                match GetLastError() {
                    ERROR_PIPE_CONNECTED | ERROR_NO_DATA => return Ok(true),
                    ERROR_IO_PENDING => {}
                    _ => return Err(io::Error::last_os_error()),
                }
            }
            while WaitForSingleObject(event.as_raw_handle(), 1000) == WAIT_TIMEOUT {
                if give_up() {
                    CancelIoEx(h, &ov);
                    break;
                }
            }
            let mut n = 0;
            if GetOverlappedResult(h, &ov, &mut n, 1) != 0 {
                return Ok(true);
            }
            match GetLastError() {
                ERROR_OPERATION_ABORTED => Ok(false),
                ERROR_NO_DATA => Ok(true),
                _ => Err(io::Error::last_os_error()),
            }
        }
    }

    /// Runs one overlapped read or write and waits for it, at most `wait_ms` (TimedOut after, the operation cancelled).
    /// The bytes moved.
    fn io(
        &self,
        wait_ms: u32,
        op: impl FnOnce(HANDLE, *mut OVERLAPPED) -> BOOL,
    ) -> io::Result<usize> {
        let h = self.0.as_raw_handle();
        // SAFETY: the OVERLAPPED, its event and the caller's buffer live until GetOverlappedResult has waited.
        unsafe {
            let mut ov: OVERLAPPED = std::mem::zeroed();
            ov.hEvent = CreateEventW(null(), 1, 0, null());
            if ov.hEvent.is_null() {
                return Err(io::Error::last_os_error());
            }
            let _event = OwnedHandle::from_raw_handle(ov.hEvent);
            if op(h, &mut ov) == 0 && GetLastError() != ERROR_IO_PENDING {
                return Err(io::Error::last_os_error());
            }
            if WaitForSingleObject(ov.hEvent, wait_ms) == WAIT_TIMEOUT {
                CancelIoEx(h, &ov);
            }
            let mut n = 0;
            if GetOverlappedResult(h, &ov, &mut n, 1) == 0 {
                return Err(match GetLastError() {
                    ERROR_OPERATION_ABORTED => io::ErrorKind::TimedOut.into(),
                    _ => io::Error::last_os_error(),
                });
            }
            Ok(n as usize)
        }
    }

    /// Like `read`, but TimedOut, nothing read, when no byte comes within `timeout`.
    pub fn read_timeout(&self, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
        let wait_ms = timeout.as_millis().min(u128::from(u32::MAX - 1)) as u32;
        self.read_within(buf, wait_ms)
    }

    /// Ok(0) once the other end has closed.
    fn read_within(&self, buf: &mut [u8], wait_ms: u32) -> io::Result<usize> {
        let len = buf.len().min(u32::MAX as usize) as u32;
        // SAFETY: `buf` is valid for `len` bytes until `io` returns.
        let read = self.io(wait_ms, |h, ov| unsafe {
            ReadFile(h, buf.as_mut_ptr(), len, null_mut(), ov)
        });
        match read {
            Err(e)
                if [ERROR_BROKEN_PIPE, ERROR_PIPE_NOT_CONNECTED]
                    .map(|c| Some(c as i32))
                    .contains(&e.raw_os_error()) =>
            {
                Ok(0)
            }
            read => read,
        }
    }
}

impl Read for &Pipe {
    /// Ok(0) once the other end has closed.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.read_within(buf, INFINITE)
    }
}

impl Write for &Pipe {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let len = buf.len().min(u32::MAX as usize) as u32;
        // SAFETY: `buf` is valid for `len` bytes until `io` returns.
        self.io(INFINITE, |h, ov| unsafe {
            WriteFile(h, buf.as_ptr(), len, null_mut(), ov)
        })
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Write for Pipe {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        (&*self).write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Checks that process `pid`, the other end of a pipe, runs this same quen executable as this same user.
pub fn check_peer(pid: u32) -> io::Result<()> {
    // SAFETY: the handle is owned at once; the buffer and its length match.
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return Err(io::Error::last_os_error());
        }
        let process = OwnedHandle::from_raw_handle(process);
        let mut buf = vec![0u16; 32 * 1024];
        let mut len = buf.len() as u32;
        check(QueryFullProcessImageNameW(
            process.as_raw_handle(),
            PROCESS_NAME_WIN32,
            buf.as_mut_ptr(),
            &mut len,
        ))?;
        let image = PathBuf::from(OsString::from_wide(&buf[..len as usize]));
        if fs::canonicalize(&image)? != fs::canonicalize(env::current_exe()?)? {
            return Err(io::Error::other(format!(
                "pid {pid} runs {}, not this quen",
                image.display()
            )));
        }
        if process_user(process.as_raw_handle())? != process_user(GetCurrentProcess())? {
            return Err(io::Error::other(format!("pid {pid} runs as another user")));
        }
    }
    Ok(())
}

/// The user a process runs as, as a SID string.
fn process_user(process: HANDLE) -> io::Result<String> {
    // SAFETY: the token is owned at once; the buffer holds the TOKEN_USER GetTokenInformation sized; the SID string
    // is freed after it is copied.
    unsafe {
        let mut token = null_mut();
        check(OpenProcessToken(process, TOKEN_QUERY, &mut token))?;
        let token = OwnedHandle::from_raw_handle(token);
        let mut size = 0;
        GetTokenInformation(token.as_raw_handle(), TokenUser, null_mut(), 0, &mut size);
        let mut buf = vec![0usize; (size as usize).div_ceil(size_of::<usize>())];
        check(GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            buf.as_mut_ptr().cast(),
            size,
            &mut size,
        ))?;
        let user = &*buf.as_ptr().cast::<TOKEN_USER>();
        let mut sid = null_mut();
        check(ConvertSidToStringSidW(user.User.Sid, &mut sid))?;
        let len = (0..).take_while(|&i| *sid.add(i) != 0).count();
        let text = String::from_utf16_lossy(std::slice::from_raw_parts(sid, len));
        LocalFree(sid.cast());
        Ok(text)
    }
}

/// A named mutex this thread owns: one `quen mcp` at a time finds or starts the daemon, and the daemon decides to exit
/// under it. Released on drop, or by Windows when the owner dies (abandoned).
pub struct NamedMutex(HANDLE);

impl NamedMutex {
    /// Waits up to `timeout` to own `name` (`Local\...`). `None` on timeout. One a dead process left counts as owned.
    pub fn acquire(name: &str, timeout: Duration) -> io::Result<Option<NamedMutex>> {
        let ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX - 1);
        // SAFETY: a valid name; the handle is closed on every path.
        unsafe {
            let handle = CreateMutexW(null(), 0, wide(name.as_ref())?.as_ptr());
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            match WaitForSingleObject(handle, ms) {
                WAIT_OBJECT_0 | WAIT_ABANDONED => Ok(Some(NamedMutex(handle))),
                WAIT_TIMEOUT => {
                    CloseHandle(handle);
                    Ok(None)
                }
                _ => {
                    let err = io::Error::last_os_error();
                    CloseHandle(handle);
                    Err(err)
                }
            }
        }
    }
}

impl Drop for NamedMutex {
    fn drop(&mut self) {
        // SAFETY: owned by this thread (a NamedMutex never moves between threads: HANDLE is !Send), closed once.
        unsafe {
            ReleaseMutex(self.0);
            CloseHandle(self.0);
        }
    }
}

/// Keeps the PC and its display awake while it lives: a thread of its own holds
/// `ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED`, a state Windows keeps per thread and releases when the
/// thread exits. The thread clears it and exits when this drops, and dies with quen.
pub struct Awake {
    stop: Option<mpsc::Sender<()>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Awake {
    pub fn hold() -> Awake {
        let (stop, wait) = mpsc::channel::<()>();
        let thread = thread::Builder::new().name("quen-awake".into()).spawn(move || {
            // SAFETY: a call with flags only.
            if unsafe { SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED) } == 0 {
                eprintln!("quen: SetThreadExecutionState failed: the display may sleep during the session");
            }
            let _ = wait.recv(); // until the Awake drops
            // SAFETY: as above; the state goes with the thread anyway.
            unsafe { SetThreadExecutionState(ES_CONTINUOUS) };
        });
        if let Err(e) = &thread {
            eprintln!("quen: no thread to keep the display awake: {e}");
        }
        Awake {
            stop: Some(stop),
            thread: thread.ok(),
        }
    }
}

impl Drop for Awake {
    fn drop(&mut self) {
        self.stop.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// A process quen started in a job, or opened by pid.
pub struct Child {
    process: HANDLE,
}

// SAFETY: a process handle may be waited on from any thread.
unsafe impl Send for Child {}

impl Child {
    /// Process `pid`, held open so its exit time stays readable after it exits.
    pub fn open(pid: u32) -> io::Result<Child> {
        let access = PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE;
        // SAFETY: a handle this Child owns and closes, or null.
        let process = unsafe { OpenProcess(access, 0, pid) };
        if process.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(Child { process })
    }

    /// When it exited, if it has.
    pub fn exited(&self) -> Option<SystemTime> {
        if !matches!(self.wait(Duration::ZERO), Ok(Some(_))) {
            return None;
        }
        let [_, ticks] = self.times()?;
        system_time(ticks)
    }

    /// When it started, in 100 ns ticks since 1601.
    pub fn started(&self) -> Option<u64> {
        self.times().map(|[started, _]| started)
    }

    /// When it started.
    pub fn started_at(&self) -> Option<SystemTime> {
        system_time(self.started()?)
    }

    /// When it started and exited (0 while it runs), in 100 ns ticks since 1601 (a FILETIME).
    fn times(&self) -> Option<[u64; 2]> {
        // SAFETY: a valid process handle and four FILETIMEs the call fills.
        let t = unsafe {
            let mut t: [FILETIME; 4] = std::mem::zeroed();
            let [created, exit, kernel, user] = &mut t;
            check(GetProcessTimes(self.process, created, exit, kernel, user)).ok()?;
            t
        };
        Some([t[0], t[1]].map(|f| (u64::from(f.dwHighDateTime) << 32) | u64::from(f.dwLowDateTime)))
    }

    /// The exit code once it has exited, or `None` if it still runs after `timeout`.
    pub fn wait(&self, timeout: Duration) -> io::Result<Option<u32>> {
        let ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX - 1);
        // SAFETY: a valid process handle.
        match unsafe { WaitForSingleObject(self.process, ms) } {
            WAIT_OBJECT_0 => {
                let mut code = 0;
                check(unsafe { GetExitCodeProcess(self.process, &mut code) })?;
                Ok(Some(code))
            }
            WAIT_TIMEOUT => Ok(None),
            _ => Err(io::Error::last_os_error()),
        }
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        // SAFETY: closed once.
        unsafe { CloseHandle(self.process) };
    }
}

/// `s` NUL-terminated, refusing an interior NUL, which would end it early (and in an environment block, start another
/// variable).
fn wide(s: &std::ffi::OsStr) -> io::Result<Vec<u16>> {
    let mut wide: Vec<u16> = s.encode_wide().collect();
    if wide.contains(&0) {
        let why = format!("{s:?} holds a NUL character");
        return Err(io::Error::new(io::ErrorKind::InvalidInput, why));
    }
    wide.push(0);
    Ok(wide)
}

/// The command line the MSVC runtime (and Python) splits back into `argv`.
fn command_line(argv: &[String]) -> io::Result<Vec<u16>> {
    let mut line = String::new();
    for arg in argv {
        if !line.is_empty() {
            line.push(' ');
        }
        if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '"']) {
            line.push_str(arg);
            continue;
        }
        line.push('"');
        let mut backslashes = 0;
        for c in arg.chars() {
            if c == '\\' {
                backslashes += 1;
                continue;
            }
            let n = if c == '"' {
                backslashes * 2 + 1
            } else {
                backslashes
            };
            line.extend(repeat_n('\\', n));
            line.push(c);
            backslashes = 0;
        }
        line.extend(repeat_n('\\', backslashes * 2));
        line.push('"');
    }
    wide(line.as_ref())
}

/// The only variables of quen's environment a child gets (a trailing `*` matches a prefix): what Windows programs
/// expect, and `QUEN_*`, quen's own (its config path and test switches), for quen's own daemon only (never
/// the game, its mods or a tool). Anything else, an API key or token in the agent's environment above all, never
/// reaches the game, its mods, a profiler, PresentMon or a script.
/// What a child needs besides comes as `extra`: the game's W3_QUEN_TOKEN and W3_QUEN_PID, the harness's Python
/// switches.
/// Nothing is added for Steam: a direct launch reads the app id from the steam_appid.txt quen places beside the exe,
/// and steam_api finds the running client through the registry, not the environment [I]. The first live boot with
/// this list confirms the game still starts.
const CHILD_ENV: &[&str] = &[
    "SystemRoot",
    "windir",
    "SystemDrive",
    "ComSpec",
    "PATHEXT",
    "PATH",
    "TEMP",
    "TMP",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "ProgramData",
    "ALLUSERSPROFILE",
    "PUBLIC",
    "ProgramFiles*",
    "ProgramW6432",
    "CommonProgram*",
    "PROCESSOR_*",
    "NUMBER_OF_PROCESSORS",
    "OS",
    "HOMEDRIVE",
    "HOMEPATH",
    "USERNAME",
    "QUEN_*",
];

/// [`CHILD_ENV`] from quen's environment (`QUEN_*` only with `quen`) plus `extra` (names compare case-insensitively),
/// sorted, as CreateProcessW takes it.
fn env_block(extra: &[(&str, &str)], quen: bool) -> io::Result<Vec<u16>> {
    let passes = |name: &str| {
        (quen || !name.starts_with("QUEN_"))
            && CHILD_ENV.iter().any(|allowed| {
                let allowed = allowed.to_uppercase();
                match allowed.strip_suffix('*') {
                    Some(prefix) => name.starts_with(prefix),
                    None => name == allowed,
                }
            })
    };
    let mut vars: BTreeMap<String, (OsString, OsString)> = env::vars_os()
        .map(|(k, v)| (k.to_string_lossy().to_uppercase(), (k, v)))
        .filter(|(name, _)| passes(name))
        .collect();
    for (k, v) in extra {
        vars.insert(k.to_uppercase(), ((*k).into(), (*v).into()));
    }
    let mut block = Vec::new();
    for (k, v) in vars.values() {
        let mut var = k.clone();
        var.push("=");
        var.push(v);
        block.extend(wide(&var)?);
    }
    block.push(0);
    Ok(block)
}

/// Runs `f` on Ctrl+C, Ctrl+Break or the console closing, on the handler thread Windows creates. `f` should end the
/// process; if it returns, the event counts as handled.
pub fn on_console_ctrl(f: fn()) {
    static HANDLER: OnceLock<fn()> = OnceLock::new();
    unsafe extern "system" fn handler(_event: u32) -> BOOL {
        if let Some(f) = HANDLER.get() {
            f();
        }
        1
    }
    if HANDLER.set(f).is_ok() {
        // SAFETY: a valid handler routine for the life of the process.
        unsafe { SetConsoleCtrlHandler(Some(handler), 1) };
    }
}

/// `n` random bytes from the system RNG, as lowercase hex.
pub fn random_hex(n: usize) -> io::Result<String> {
    let mut bytes = vec![0u8; n];
    // SAFETY: the buffer and its length match.
    let status = unsafe {
        BCryptGenRandom(
            null_mut(),
            bytes.as_mut_ptr(),
            n as u32,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    if status != 0 {
        return Err(io::Error::other(format!(
            "BCryptGenRandom failed: {status:#x}"
        )));
    }
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// The system tick count in ms: the clock quen's state log stamps its lines with.
/// A FILETIME's 100 ns ticks since 1601 as a time.
fn system_time(ticks: u64) -> Option<SystemTime> {
    let since_1601 = Duration::from_nanos(ticks.saturating_mul(100));
    (UNIX_EPOCH + since_1601).checked_sub(Duration::from_secs(11_644_473_600))
}

pub fn tick() -> u64 {
    // SAFETY: a plain query.
    unsafe { GetTickCount64() }
}

/// The full name of the app package this process runs in (Claude Desktop's MSIX, and every process it starts), if any:
/// Windows then redirects its AppData writes into the package's private folder (relocate.rs).
pub fn package() -> Option<String> {
    let mut buf = [0u16; 256];
    let mut len = buf.len() as u32;
    // SAFETY: a buffer and its length in characters; APPMODEL_ERROR_NO_PACKAGE outside a package.
    let err = unsafe { GetCurrentPackageFullName(&mut len, buf.as_mut_ptr()) };
    let name = &buf[..(len as usize).saturating_sub(1).min(buf.len())];
    (err == 0).then(|| String::from_utf16_lossy(name))
}

/// The user's Documents folder, wherever it was moved or redirected to (OneDrive included).
pub fn documents() -> Option<PathBuf> {
    known_folder(&FOLDERID_Documents)
}

/// The user's Pictures folder, wherever it was moved or redirected to.
pub fn pictures() -> Option<PathBuf> {
    known_folder(&FOLDERID_Pictures)
}

fn known_folder(id: &GUID) -> Option<PathBuf> {
    let mut path = null_mut();
    // SAFETY: on success `path` is a NUL-terminated string Windows allocated; it is copied, then freed once.
    unsafe {
        let ok = SHGetKnownFolderPath(id, 0, null_mut(), &mut path) == 0;
        let dir = ok.then(|| {
            let len = (0..).take_while(|&i| *path.add(i) != 0).count();
            PathBuf::from(OsString::from_wide(std::slice::from_raw_parts(path, len)))
        });
        CoTaskMemFree(path.cast());
        dir
    }
}

/// An exe's or DLL's version resource, as `a.b.c.d` (its fixed file version); `None` without one.
pub fn file_version(path: &Path) -> Option<String> {
    let name = wide(path.as_os_str()).ok()?;
    // SAFETY: a NUL-terminated name; the buffer is the size Windows asked for, and VerQueryValueW points into it.
    unsafe {
        let size = GetFileVersionInfoSizeW(name.as_ptr(), null_mut());
        let mut data = vec![0u8; size as usize];
        if size == 0 || GetFileVersionInfoW(name.as_ptr(), 0, size, data.as_mut_ptr().cast()) == 0 {
            return None;
        }
        let (mut info, mut len) = (null_mut(), 0u32);
        let root = [u16::from(b'\\'), 0];
        if VerQueryValueW(data.as_ptr().cast(), root.as_ptr(), &mut info, &mut len) == 0
            || (len as usize) < size_of::<VS_FIXEDFILEINFO>()
        {
            return None;
        }
        let info = &*info.cast::<VS_FIXEDFILEINFO>();
        let (ms, ls) = (info.dwFileVersionMS, info.dwFileVersionLS);
        Some(format!(
            "{}.{}.{}.{}",
            ms >> 16,
            ms & 0xFFFF,
            ls >> 16,
            ls & 0xFFFF
        ))
    }
}

/// Renames `from` to `to` on one volume, never over anything: MoveFileExW without `MOVEFILE_REPLACE_EXISTING` (which
/// std's rename sets) and without `MOVEFILE_COPY_ALLOWED`, so a file id survives and a move across volumes fails.
pub fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    move_file(from, to, MOVEFILE_WRITE_THROUGH)
}

/// Renames `from` over `to` (a temp file onto the file it replaces) with `MOVEFILE_WRITE_THROUGH`: the journal's
/// atomic write (std's rename doesn't ask for it).
pub fn rename_over(from: &Path, to: &Path) -> io::Result<()> {
    move_file(from, to, MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)
}

/// How long a rename is retried while another process holds the file: an antivirus scan or the indexer opens a file
/// quen just wrote for milliseconds (the session tests under load: 44-221 ms, every one through).
const MOVE_RETRY: Duration = Duration::from_secs(2);

/// MoveFileExW with `flags`, retried for up to [`MOVE_RETRY`] while it fails with access denied or a sharing violation:
/// what another process holding `from` or `to` open without delete sharing causes. Without it, the session tests under
/// load had launches refused ("Access is denied") and an end that left its journal: the boot closed as recovered only
/// by the daemon's exit retry.
fn move_file(from: &Path, to: &Path, flags: u32) -> io::Result<()> {
    let (from, to) = (wide(from.as_os_str())?, wide(to.as_os_str())?);
    let started = std::time::Instant::now();
    loop {
        // SAFETY: two NUL-terminated paths that outlive the call.
        let moved = check(unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), flags) });
        let held = [ERROR_ACCESS_DENIED, ERROR_SHARING_VIOLATION]
            .map(|c| Some(c as i32))
            .contains(&moved.as_ref().err().and_then(io::Error::raw_os_error));
        if !held || started.elapsed() > MOVE_RETRY {
            return moved;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The volume serial and file index of a file or folder (a link itself, not its target): what a rename on one volume
/// keeps.
pub fn file_id(path: &Path) -> io::Result<String> {
    file_info(path).map(|(id, _)| id)
}

/// [`file_id`], and how many names the file has (more than one: a hardlink).
pub fn file_info(path: &Path) -> io::Result<(String, u32)> {
    let name = wide(path.as_os_str())?;
    // SAFETY: a NUL-terminated path; the handle is owned and closed once; the info struct is the documented layout.
    unsafe {
        let handle = CreateFileW(
            name.as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            null_mut(),
        );
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let handle = OwnedHandle::from_raw_handle(handle);
        let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
        check(GetFileInformationByHandle(
            handle.as_raw_handle(),
            &mut info,
        ))?;
        let id = format!(
            "{:08x}-{:08x}{:08x}",
            info.dwVolumeSerialNumber, info.nFileIndexHigh, info.nFileIndexLow
        );
        Ok((id, info.nNumberOfLinks))
    }
}

/// A string value in the registry, under `HKEY_LOCAL_MACHINE` (`machine`) or `HKEY_CURRENT_USER`; `None` when absent.
pub fn reg_str(machine: bool, key: &str, value: &str) -> Option<String> {
    let root = if machine {
        HKEY_LOCAL_MACHINE
    } else {
        HKEY_CURRENT_USER
    };
    let (key, value) = (wide(key.as_ref()).ok()?, wide(value.as_ref()).ok()?);
    let mut buf = vec![0u16; 1024];
    loop {
        let mut bytes = (buf.len() * 2) as u32;
        // SAFETY: the buffer and its size in bytes match; RRF_RT_REG_SZ NUL-terminates what it returns.
        let err = unsafe {
            RegGetValueW(
                root,
                key.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_SZ,
                null_mut(),
                buf.as_mut_ptr().cast(),
                &mut bytes,
            )
        };
        match err {
            0 => {
                let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
                return Some(String::from_utf16_lossy(&buf[..len]));
            }
            ERROR_MORE_DATA => buf.resize(bytes as usize / 2 + 1, 0),
            _ => return None,
        }
    }
}

/// A DWORD value under `HKEY_LOCAL_MACHINE` (`machine`) or `HKEY_CURRENT_USER`; `None` when absent.
pub fn reg_dword(machine: bool, key: &str, value: &str) -> Option<u32> {
    let root = if machine {
        HKEY_LOCAL_MACHINE
    } else {
        HKEY_CURRENT_USER
    };
    let (key, value) = (wide(key.as_ref()).ok()?, wide(value.as_ref()).ok()?);
    let (mut data, mut bytes) = (0u32, 4u32);
    // SAFETY: a DWORD buffer and its size in bytes; RRF_RT_REG_DWORD refuses any other type.
    let err = unsafe {
        RegGetValueW(
            root,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            null_mut(),
            (&raw mut data).cast(),
            &mut bytes,
        )
    };
    (err == 0).then_some(data)
}

/// Whether `path` carries a valid embedded Authenticode signature that chains to a trusted root (WinVerifyTrust,
/// generic verify v2), checked offline: no revocation check and no URL fetched. Err says why not.
/// ponytail: the signer isn't named; callers pin the file's sha256 too, which names the bytes exactly.
pub fn authenticode(path: &Path) -> Result<(), String> {
    let name = wide(path.as_os_str()).map_err(|e| e.to_string())?;
    let mut file = WINTRUST_FILE_INFO {
        cbStruct: size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: name.as_ptr(),
        ..Default::default()
    };
    let mut data = WINTRUST_DATA {
        cbStruct: size_of::<WINTRUST_DATA>() as u32,
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_NONE,
        dwUnionChoice: WTD_CHOICE_FILE,
        Anonymous: WINTRUST_DATA_0 { pFile: &mut file },
        dwStateAction: WTD_STATEACTION_VERIFY,
        dwProvFlags: WTD_CACHE_ONLY_URL_RETRIEVAL,
        ..Default::default()
    };
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    // SAFETY: the structs and the name outlive both calls; the second releases the state the first opened.
    let status = unsafe {
        let status = WinVerifyTrust(null_mut(), &mut action, (&raw mut data).cast());
        data.dwStateAction = WTD_STATEACTION_CLOSE;
        WinVerifyTrust(null_mut(), &mut action, (&raw mut data).cast());
        status
    };
    match status {
        0 => Ok(()),
        s => Err(format!(
            "WinVerifyTrust 0x{:08X}: {}",
            s as u32,
            io::Error::from_raw_os_error(s)
        )),
    }
}

/// The names of the subkeys of `key` under `HKEY_LOCAL_MACHINE`; none when it is absent.
pub fn reg_subkeys(key: &str) -> Vec<String> {
    let Ok(key) = wide(key.as_ref()) else {
        return Vec::new();
    };
    let mut hkey = null_mut();
    // SAFETY: a NUL-terminated name; the key opened here is closed below.
    if unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, key.as_ptr(), 0, KEY_READ, &mut hkey) } != 0 {
        return Vec::new();
    }
    let mut names = Vec::new();
    let mut name = [0u16; 256]; // a key name is at most 255 characters
    for i in 0.. {
        let mut len = name.len() as u32;
        // SAFETY: the buffer and its length in characters match; the optional outputs are null.
        let err = unsafe {
            RegEnumKeyExW(
                hkey,
                i,
                name.as_mut_ptr(),
                &mut len,
                null(),
                null_mut(),
                null_mut(),
                null_mut(),
            )
        };
        if err != 0 {
            break;
        }
        names.push(String::from_utf16_lossy(&name[..len as usize]));
    }
    // SAFETY: opened above, closed once.
    unsafe { RegCloseKey(hkey) };
    names
}

/// Sends `path` (a file or a folder) to the Recycle Bin: nothing quen removes is ever deleted for good.
/// ponytail: a volume without a Recycle Bin deletes instead; only quen's own files (backups, a placed
/// steam_appid.txt) are recycled, on the data root and the game's volume, which have one. `QUEN_TEST_RECYCLE_TO`
/// (tests only) moves it into that folder instead, so test runs leave the user's Recycle Bin alone.
pub fn recycle(path: &Path) -> io::Result<()> {
    if let Some(dir) = env::var_os("QUEN_TEST_RECYCLE_TO") {
        let dir = PathBuf::from(dir);
        fs::create_dir_all(&dir)?;
        let n = fs::read_dir(&dir)?.count();
        return rename_no_replace(path, &dir.join(n.to_string()));
    }
    let (code, aborted) = shell_delete_op(path, FOF_ALLOWUNDO | FOF_NOCONFIRMATION)?;
    if code != 0 || aborted || fs::symlink_metadata(path).is_ok() {
        return Err(io::Error::other(format!(
            "{} not sent to the Recycle Bin (SHFileOperationW {code:#x})",
            path.display()
        )));
    }
    Ok(())
}

/// capture_clean's delete of `path` through the Windows shell, which asks the user before any permanent delete:
/// `recycle` sends it to the Recycle Bin, and Windows asks first if the bin can't take it (`FOF_WANTNUKEWARNING`);
/// otherwise Windows asks whether to delete it permanently. Whether it is gone: false when the user said no.
/// `QUEN_TEST_RECYCLE_TO` (tests only) takes the Recycle Bin's place, as for [`recycle`].
pub fn shell_delete(path: &Path, to_bin: bool) -> io::Result<bool> {
    if to_bin && env::var_os("QUEN_TEST_RECYCLE_TO").is_some() {
        return recycle(path).map(|()| true);
    }
    let flags = match to_bin {
        true => FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_WANTNUKEWARNING,
        false => 0,
    };
    let (code, aborted) = shell_delete_op(path, flags)?;
    match fs::symlink_metadata(path).is_err() {
        true => Ok(true),
        false if aborted || code == ERROR_CANCELLED as i32 => Ok(false),
        false => Err(io::Error::other(format!(
            "{} is still there (SHFileOperationW {code:#x})",
            path.display()
        ))),
    }
}

/// SHFileOperationW's delete of `path` with `flags`, and always `FOF_NOERRORUI` (an error comes back as its code) and
/// `FOF_SILENT` (no progress window); no owner window. Its code, and whether an operation was aborted.
fn shell_delete_op(path: &Path, flags: u32) -> io::Result<(i32, bool)> {
    let mut from = wide(std::path::absolute(path)?.as_os_str())?;
    from.push(0);
    let mut op = SHFILEOPSTRUCTW {
        hwnd: null_mut(),
        wFunc: FO_DELETE,
        pFrom: from.as_ptr(),
        pTo: null(),
        fFlags: (flags | FOF_NOERRORUI | FOF_SILENT) as u16,
        fAnyOperationsAborted: 0,
        hNameMappings: null_mut(),
        lpszProgressTitle: null(),
    };
    // SAFETY: a double-NUL-terminated path list that outlives the call; no owner window, no progress title.
    let code = unsafe { SHFileOperationW(&mut op) };
    Ok((code, op.fAnyOperationsAborted != 0))
}

/// `GetDriveTypeW`'s fixed drive (in WindowsProgramming, a feature quen doesn't use).
const DRIVE_FIXED: u32 = 3;

/// What the Recycle Bin of `path`'s volume takes whole, as the registry says (Explorer's per-volume `MaxCapacity`, MB):
/// `Ok(None)` when it doesn't say. `Err` where a recycle would delete instead: a drive that isn't fixed (removable and
/// network drives have no bin), the volume's `NukeOnDelete`, or the `NoRecycleFiles` policy. `QUEN_TEST_RECYCLE_TO`
/// (tests only, see [`recycle`]) takes anything.
pub fn recycle_bin(path: &Path) -> Result<Option<u64>, String> {
    if env::var_os("QUEN_TEST_RECYCLE_TO").is_some() {
        return Ok(None);
    }
    let policy = r"Software\Microsoft\Windows\CurrentVersion\Policies\Explorer";
    if [true, false]
        .into_iter()
        .any(|machine| reg_dword(machine, policy, "NoRecycleFiles") == Some(1))
    {
        return Err("the NoRecycleFiles policy turns the Recycle Bin off".into());
    }
    let path = wide(path.as_os_str()).map_err(|e| e.to_string())?;
    let (mut root, mut name) = ([0u16; 261], [0u16; 64]);
    // SAFETY: a NUL-terminated path and buffers whose lengths are passed.
    let found = unsafe {
        GetVolumePathNameW(path.as_ptr(), root.as_mut_ptr(), root.len() as u32) != 0
            && GetDriveTypeW(root.as_ptr()) == DRIVE_FIXED
            && GetVolumeNameForVolumeMountPointW(
                root.as_ptr(),
                name.as_mut_ptr(),
                name.len() as u32,
            ) != 0
    };
    let text = |w: &[u16]| {
        String::from_utf16_lossy(&w[..w.iter().position(|&c| c == 0).unwrap_or(w.len())])
    };
    let (root, name) = (text(&root), text(&name));
    let guid = name
        .find('{')
        .and_then(|at| Some(&name[at..=at + name[at..].find('}')?]));
    let Some(guid) = guid.filter(|_| found) else {
        return Err(format!("{root:?} is no fixed drive with a Recycle Bin"));
    };
    let key =
        format!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\BitBucket\Volume\{guid}");
    if reg_dword(false, &key, "NukeOnDelete") == Some(1) {
        return Err(format!(
            "the Recycle Bin is off on {root} (it deletes files at once)"
        ));
    }
    Ok(reg_dword(false, &key, "MaxCapacity").map(|mb| u64::from(mb) * 1_048_576))
}

/// Ends the one process `pid`, which the caller found in its session's job (never by image name).
pub fn terminate(pid: u32) -> io::Result<()> {
    // SAFETY: the handle is owned and closed once.
    unsafe {
        let process = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if process.is_null() {
            return Err(io::Error::last_os_error());
        }
        let process = OwnedHandle::from_raw_handle(process);
        check(TerminateProcess(process.as_raw_handle(), 1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unsigned bytes and a missing file fail the Authenticode check, offline, with WinVerifyTrust's reason; an absent
    /// registry value reads as none. (A file with an embedded signature can't be assumed on every Windows: system
    /// files are catalog-signed. The live renderdoc test proves the positive case on the game's interposer.)
    #[test]
    fn unsigned_files_fail_authenticode() {
        let path = env::temp_dir().join(format!("quen-unsigned-{}.dll", std::process::id()));
        fs::write(&path, b"MZ synthetic, unsigned").unwrap();
        let unsigned = authenticode(&path).unwrap_err();
        let _ = fs::remove_file(&path); // this test's own file
        assert!(unsigned.starts_with("WinVerifyTrust 0x"), "{unsigned}");
        assert!(authenticode(&path).is_err(), "a missing file");
        assert_eq!(reg_dword(true, r"SOFTWARE\quen-no-such-key", "x"), None);
    }

    #[test]
    fn command_line_quotes_as_msvc_splits() {
        let argv = [
            "python",
            r"C:\a b\ini_session.py",
            "boot",
            r#"say "hi""#,
            r"trail\",
            r"a b\",
            "",
        ]
        .map(String::from);
        let line = String::from_utf16(&command_line(&argv).unwrap()).unwrap();
        // A backslash is literal unless a quote follows it, so `trail\` needs no quotes and `a b\` doubles it.
        let want = r#"python "C:\a b\ini_session.py" boot "say \"hi\"" trail\ "a b\\" """#;
        assert_eq!(line.trim_end_matches('\0'), want);
    }

    #[test]
    fn env_block_overrides_case_insensitively_and_ends_in_two_nuls() {
        let block = env_block(&[("Quen_Test_Var", "1"), ("QUEN_TEST_VAR", "2")], false).unwrap();
        let text = String::from_utf16(&block).unwrap();
        assert!(text.ends_with("\0\0"));
        let hits: Vec<&str> = text
            .split('\0')
            .filter(|v| v.to_uppercase().starts_with("QUEN_TEST_VAR="))
            .collect();
        assert_eq!(hits, ["QUEN_TEST_VAR=2"]);
    }

    /// A child sees only the allow-list and its extras: no secret of the agent's environment.
    #[test]
    fn a_child_gets_no_secrets_from_quens_environment() {
        // Audit V6: quen's own variables (a deny-list's path, say) are the daemon's only.
        // SAFETY: always safe on Windows (std's docs); the name is this test's own.
        unsafe { env::set_var("QUEN_V6_PROBE", "x") };
        let secrets = [
            "QUENTEST_API_KEY",
            "SOME_TOKEN_QUENTEST",
            "GITHUB_QUENTEST",
            "ANTHROPIC_QUENTEST",
            "OPENAI_QUENTEST",
        ];
        for name in secrets {
            // SAFETY: always safe on Windows (std's docs); these names are this test's own.
            unsafe { env::set_var(name, "secret") };
        }
        let job = Job::new().unwrap();
        let dir = env::temp_dir();
        let path = dir.join(format!("quen-env-test-{}.log", std::process::id()));
        let log = File::create(&path).unwrap();
        let argv = ["cmd", "/c", "set"].map(String::from);
        let child = job
            .spawn(&argv, &[("W3_QUEN_PID", "1")], &dir, &log)
            .unwrap();
        assert_eq!(child.wait(Duration::from_secs(10)).unwrap(), Some(0));
        drop(log);
        let out = fs::read_to_string(&path).unwrap();
        let names: Vec<String> = out
            .lines()
            .filter_map(|l| l.split_once('='))
            .map(|(k, _)| k.to_uppercase())
            .collect();
        let secret = |n: &&String| {
            n.ends_with("_API_KEY")
                || n.contains("TOKEN")
                || ["GITHUB_", "ANTHROPIC_", "OPENAI_"]
                    .iter()
                    .any(|p| n.starts_with(p))
        };
        assert_eq!(names.iter().find(secret), None, "{out}");
        assert!(!names.iter().any(|n| n.starts_with("QUEN_")), "{out}");
        for want in ["SYSTEMROOT", "PATH", "W3_QUEN_PID"] {
            assert!(names.iter().any(|n| n == want), "{want} missing: {out}");
        }
        fs::remove_file(path).unwrap(); // this test's own temp file
    }

    #[test]
    fn an_interior_nul_is_refused_not_split() {
        let invalid =
            |r: io::Result<Vec<u16>>| r.unwrap_err().kind() == io::ErrorKind::InvalidInput;
        assert!(invalid(env_block(&[("W3_SIGNALS", "x\0PATH=evil")], false)));
        assert!(invalid(env_block(&[("W3_NGFX_\0PATH", "x")], false)));
        assert!(invalid(command_line(&["boot".into(), "a\0b".into()])));
    }

    #[test]
    fn job_kills_its_processes_and_reports_them() {
        let job = Job::new().unwrap();
        let dir = env::temp_dir();
        let path = dir.join(format!("quen-job-test-{}.log", std::process::id()));
        let log = File::create(&path).unwrap();
        // ping waits about a second per count: a stand-in for the game.
        let argv = ["ping", "-n", "30", "127.0.0.1"].map(String::from);
        let child = job.spawn(&argv, &[], &dir, &log).unwrap();
        assert_eq!(child.wait(Duration::from_millis(200)).unwrap(), None);
        // Held open by pid, as state.game holds the game: no exit time until it exits.
        let held = Child::open(job.pids().unwrap()[0]).unwrap();
        assert_eq!(held.exited(), None);
        job.kill().unwrap();
        assert_eq!(child.wait(Duration::from_secs(5)).unwrap(), Some(1));
        assert!(job.pids().unwrap().is_empty());
        let ago = SystemTime::now().duration_since(held.exited().unwrap());
        assert!(ago.as_ref().unwrap() < &Duration::from_secs(10), "{ago:?}");
        drop(log);
        std::fs::remove_file(path).unwrap(); // this test's own temp file
    }

    /// Store Python (a packaged app) starts other programs outside its container, and so outside the job, unless told
    /// not to. With any other Python this passes trivially.
    #[test]
    fn what_python_starts_stays_in_the_job() {
        let job = Job::new().unwrap();
        let dir = env::temp_dir();
        let path = dir.join(format!("quen-job-python-{}.log", std::process::id()));
        let log = File::create(&path).unwrap();
        let code = "import subprocess, time; p = subprocess.Popen(['ping', '-n', '30', '127.0.0.1'], \
                    stdout=subprocess.DEVNULL); print('pid', p.pid, flush=True); time.sleep(30)";
        job.spawn(&["python", "-c", code].map(String::from), &[], &dir, &log)
            .unwrap();
        let start = std::time::Instant::now();
        let pid = loop {
            let out = fs::read_to_string(&path).unwrap();
            if let Some(pid) = out.split_whitespace().nth(1) {
                break pid.parse::<u32>().unwrap();
            }
            assert!(
                start.elapsed() < Duration::from_secs(20),
                "python said nothing"
            );
            std::thread::sleep(Duration::from_millis(50));
        };
        assert!(
            job.pids().unwrap().contains(&pid),
            "ping {pid} runs outside the job"
        );
        job.kill().unwrap();
        drop(log);
        std::fs::remove_file(path).unwrap(); // this test's own temp file
    }

    #[test]
    fn renames_never_replace_and_keep_the_file_id() {
        let dir = env::temp_dir().join(format!("quen-win-rename-{}", std::process::id()));
        fs::create_dir_all(dir.join("folder")).unwrap();
        let (a, b, c) = (dir.join("a"), dir.join("b"), dir.join("c"));
        fs::write(&a, "a").unwrap();
        fs::write(&c, "c").unwrap();
        let id = file_id(&a).unwrap();
        rename_no_replace(&a, &b).unwrap();
        assert_eq!(file_id(&b).unwrap(), id);
        let err = rename_no_replace(&b, &c).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists, "{err}");
        assert_eq!(fs::read_to_string(&c).unwrap(), "c");
        let folder = file_id(&dir.join("folder")).unwrap();
        rename_no_replace(&dir.join("folder"), &dir.join("parked")).unwrap();
        assert_eq!(file_id(&dir.join("parked")).unwrap(), folder);
        assert_ne!(folder, id);
        fs::remove_dir_all(dir).unwrap(); // this test's own temp folder
    }

    /// A rename waits out another process holding the file without delete sharing, as an antivirus scan does for
    /// milliseconds (the session tests' flaky "Access is denied"); a holder that stays makes it fail after the retry.
    #[test]
    fn a_rename_waits_for_a_brief_holder_of_the_file() {
        let dir = env::temp_dir().join(format!("quen-win-held-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let (tmp, file) = (dir.join("file.quen-tmp"), dir.join("file"));
        fs::write(&file, "old").unwrap();
        let hold = || {
            OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_READ)
                .open(&file)
                .unwrap()
        };
        fs::write(&tmp, "new").unwrap();
        let held = hold();
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            drop(held);
        });
        rename_over(&tmp, &file).unwrap();
        release.join().unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "new");
        fs::write(&tmp, "newer").unwrap();
        let held = hold();
        let t = std::time::Instant::now();
        let err = rename_over(&tmp, &file).unwrap_err();
        assert!(t.elapsed() >= MOVE_RETRY, "{err}");
        drop(held);
        fs::remove_dir_all(dir).unwrap(); // this test's own temp folder
    }

    /// The real Recycle Bin, so it runs only when asked: `cargo test -p quen recycle -- --ignored`.
    #[test]
    #[ignore = "sends a synthetic file and folder to the Recycle Bin"]
    fn recycle_sends_files_and_folders_to_the_recycle_bin() {
        let dir = env::temp_dir().join(format!("quen-recycle-test-{}", std::process::id()));
        fs::create_dir_all(dir.join("folder")).unwrap();
        fs::write(dir.join("file.txt"), "x").unwrap();
        fs::write(dir.join("folder").join("inner.txt"), "y").unwrap();
        recycle(&dir.join("file.txt")).unwrap();
        recycle(&dir.join("folder")).unwrap();
        assert!(!dir.join("file.txt").exists() && !dir.join("folder").exists());
        assert!(recycle(&dir.join("missing")).is_err());
        fs::remove_dir(dir).unwrap(); // empty by now
    }

    #[test]
    fn random_hex_is_fresh() {
        let (a, b) = (random_hex(16).unwrap(), random_hex(16).unwrap());
        assert_eq!(a.len(), 32);
        assert_ne!(a, b);
    }
}
