use anyhow::{ensure, Context, Result};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    io::Read,
    mem::size_of,
    os::windows::{
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Child, Command},
    sync::{Mutex, OnceLock},
};
use windows::{
    core::{HSTRING, PCWSTR},
    Win32::{Foundation::HANDLE, Storage::FileSystem::GetDiskFreeSpaceExW, System::JobObjects::*},
};

pub fn command(exe: &Path) -> Command {
    let mut command = Command::new(exe);
    command.creation_flags(0x08000000);
    command.stdout(std::process::Stdio::null());
    command
}

pub fn publish_new_file(temporary: &Path, destination: &Path) -> Result<()> {
    use windows::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_WRITE_THROUGH};
    let staging_parent = std::fs::canonicalize(
        temporary
            .parent()
            .context("Staged output has no parent directory")?,
    )?;
    let parent = std::fs::canonicalize(
        destination
            .parent()
            .context("Final output has no parent directory")?,
    )?;
    ensure!(
        staging_parent == parent,
        "Output must be published within its staging directory."
    );
    let source = staging_parent.join(
        temporary
            .file_name()
            .context("Staged filename is missing")?,
    );
    let destination = parent.join(
        destination
            .file_name()
            .context("Final filename is missing")?,
    );
    // SAFETY: Extended-length UTF-16 paths remain live; no REPLACE_EXISTING flag is supplied.
    unsafe {
        MoveFileExW(&HSTRING::from(source.as_os_str()), &HSTRING::from(destination.as_os_str()), MOVEFILE_WRITE_THROUGH)
    }.context("Publishing the completed file without overwriting existing content; the library original or committed segments remain available")
}

pub struct Job {
    handle: OwnedHandle,
}
impl Job {
    pub fn new() -> Result<Self> {
        // SAFETY: The fresh, noninheritable handle transfers to exactly one OwnedHandle.
        unsafe {
            let raw = CreateJobObjectW(None, PCWSTR::null())?;
            let handle = OwnedHandle::from_raw_handle(raw.0);
            let job = Self { handle };
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                HANDLE(job.handle.as_raw_handle()),
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const _,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )?;
            Ok(job)
        }
    }
    pub fn assign(&self, child: &Child) -> Result<()> {
        // SAFETY: Both borrowed handles outlive the call; no ownership is transferred.
        unsafe {
            AssignProcessToJobObject(
                HANDLE(self.handle.as_raw_handle()),
                HANDLE(child.as_raw_handle()),
            )?;
        }
        Ok(())
    }
    pub fn terminate(&self) -> Result<()> {
        // SAFETY: This handle exclusively owns the app's worker job, not unrelated processes.
        unsafe {
            TerminateJobObject(HANDLE(self.handle.as_raw_handle()), 3)?;
        }
        Ok(())
    }
    pub fn active_processes(&self) -> Result<u32> {
        let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        // SAFETY: The output buffer and explicit byte size describe the same live struct.
        unsafe {
            QueryInformationJobObject(
                HANDLE(self.handle.as_raw_handle()),
                JobObjectBasicAccountingInformation,
                &mut info as *mut _ as *mut _,
                size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                None,
            )?;
        }
        Ok(info.ActiveProcesses)
    }
    pub fn peak_committed_bytes(&self) -> Result<usize> {
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        // SAFETY: The live output struct matches the queried information class and byte size.
        unsafe {
            QueryInformationJobObject(
                HANDLE(self.handle.as_raw_handle()),
                JobObjectExtendedLimitInformation,
                &mut info as *mut _ as *mut _,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                None,
            )?;
        }
        Ok(info.PeakJobMemoryUsed)
    }
    pub fn cpu_100ns(&self) -> Result<u64> {
        let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        // SAFETY: The buffer matches the queried information class for this owned job.
        unsafe {
            QueryInformationJobObject(
                HANDLE(self.handle.as_raw_handle()),
                JobObjectBasicAccountingInformation,
                &mut info as *mut _ as *mut _,
                size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                None,
            )?;
        }
        ensure!(
            info.TotalKernelTime >= 0 && info.TotalUserTime >= 0,
            "Invalid process-family CPU accounting."
        );
        Ok(info.TotalKernelTime as u64 + info.TotalUserTime as u64)
    }
}

#[derive(Clone, Copy, serde::Serialize)]
pub struct ProcessStats {
    pub cpu_100ns: u64,
    pub private_bytes: usize,
    pub system_idle_100ns: u64,
    pub system_total_100ns: u64,
}

fn filetime_ticks(time: windows::Win32::Foundation::FILETIME) -> u64 {
    (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)
}

pub fn current_process_stats() -> Result<ProcessStats> {
    use windows::Win32::{
        Foundation::FILETIME,
        System::{
            ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX},
            Threading::{GetCurrentProcess, GetProcessTimes, GetSystemTimes},
        },
    };
    let (mut created, mut exited, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    let (mut idle, mut system_kernel, mut system_user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    let mut memory = PROCESS_MEMORY_COUNTERS_EX {
        cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        ..Default::default()
    };
    // SAFETY: Read only this process's counters and aggregate system counters into correctly sized storage.
    unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut created,
            &mut exited,
            &mut kernel,
            &mut user,
        )?;
        GetSystemTimes(
            Some(&mut idle),
            Some(&mut system_kernel),
            Some(&mut system_user),
        )?;
        K32GetProcessMemoryInfo(
            GetCurrentProcess(),
            (&mut memory as *mut PROCESS_MEMORY_COUNTERS_EX).cast(),
            memory.cb,
        )
        .ok()?;
    }
    Ok(ProcessStats {
        cpu_100ns: filetime_ticks(kernel) + filetime_ticks(user),
        private_bytes: memory.PrivateUsage,
        system_idle_100ns: filetime_ticks(idle),
        system_total_100ns: filetime_ticks(system_kernel) + filetime_ticks(system_user),
    })
}

pub fn free_space(path: &Path) -> Result<u64> {
    let mut free = 0;
    // SAFETY: The UTF-16 path and writable scalar remain alive through this synchronous call.
    unsafe {
        GetDiskFreeSpaceExW(
            &HSTRING::from(path.as_os_str()),
            Some(&mut free),
            None,
            None,
        )?;
    }
    Ok(free)
}

pub fn restore_owned_window(raw: isize) -> Result<()> {
    use windows::Win32::{
        Foundation::HWND,
        UI::WindowsAndMessaging::{GetWindowThreadProcessId, ShowWindowAsync, SW_SHOWNOACTIVATE},
    };
    let hwnd = HWND(raw as *mut std::ffi::c_void);
    let mut process_id = 0;
    // SAFETY: Validate ownership before posting a nonactivating restore to this app's own window.
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut process_id));
        ensure!(
            process_id == std::process::id(),
            "Capture editor window is no longer owned by this process."
        );
        ShowWindowAsync(hwnd, SW_SHOWNOACTIVATE).ok()?;
    }
    Ok(())
}

pub fn media_tool(name: &str) -> Result<PathBuf> {
    ensure!(name == "ffmpeg" || name == "ffprobe", "Invalid media tool.");
    let executable = std::env::current_exe()?;
    let adjacent = executable
        .parent()
        .context("Executable has no directory")?
        .join("tools")
        .join(format!("{name}.exe"));
    if adjacent.is_file() {
        verify_tool(name, &adjacent)?;
        return Ok(adjacent);
    }
    let variable = format!("SNAPPER_{}", name.to_ascii_uppercase());
    let legacy = format!("STEADYCAPTURE_{}", name.to_ascii_uppercase());
    if let Some(value) = std::env::var_os(variable).or_else(|| std::env::var_os(legacy)) {
        let path = PathBuf::from(value);
        ensure!(
            path.is_absolute() && path.is_file(),
            "Configured {name} is not an absolute executable path."
        );
        verify_tool(name, &path)?;
        return Ok(path);
    }
    anyhow::bail!("The pinned {name} executable is missing. Run scripts\\Bootstrap.ps1 then scripts\\Package.ps1; do not download an unverified replacement.");
}

fn verify_tool(name: &str, path: &Path) -> Result<()> {
    static VERIFIED: OnceLock<Mutex<HashMap<PathBuf, File>>> = OnceLock::new();
    let mut verified = VERIFIED
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .map_err(|_| anyhow::anyhow!("Codec provenance cache is unavailable"))?;
    if verified.contains_key(path) {
        return Ok(());
    }
    ensure!(
        verified.len() < 4,
        "Too many codec locations were requested in one process."
    );
    let expected = if name == "ffmpeg" {
        "3256173f3f8bffd7df12227c68adf68025edb1832273a9530688a7bb1ed8edec"
    } else {
        "f0d36ecbbdd3bcfac3efa078c96c7271c2e68b3810595552ac3b7f17e9a65c52"
    };
    let file = verified_file(path, expected).with_context(|| {
        format!("Pinned {name} verification failed. Re-run the verified bootstrap/package steps.")
    })?;
    verified.insert(path.to_owned(), file);
    Ok(())
}

fn verified_file(path: &Path, expected: &str) -> Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    let mut file = OpenOptions::new().read(true).share_mode(1).open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let length = file.read(&mut buffer)?;
        if length == 0 {
            break;
        }
        digest.update(&buffer[..length]);
    }
    ensure!(
        format!("{:x}", digest.finalize()) == expected,
        "SHA-256 does not match. The unverified file was not executed."
    );
    Ok(file)
}

pub fn descendants(parent: u32) -> Result<Vec<u32>> {
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    // SAFETY: Snapshot and output structs are owned locally; only descendants of our PID are returned.
    unsafe {
        let snapshot =
            OwnedHandle::from_raw_handle(CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)?.0);
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        Process32FirstW(HANDLE(snapshot.as_raw_handle()), &mut entry)?;
        let mut pairs = Vec::new();
        loop {
            pairs.push((entry.th32ProcessID, entry.th32ParentProcessID));
            ensure!(
                pairs.len() < 32768,
                "Process snapshot exceeded its safety budget."
            );
            match Process32NextW(HANDLE(snapshot.as_raw_handle()), &mut entry) {
                Ok(()) => {}
                Err(error) if error.code() == windows::core::HRESULT::from_win32(18) => break,
                Err(error) => return Err(error.into()),
            }
        }
        let mut output = Vec::new();
        let mut parents = vec![parent];
        while let Some(parent) = parents.pop() {
            for &(id, owner) in &pairs {
                if owner == parent && !output.contains(&id) && id != parent {
                    output.push(id);
                    parents.push(id);
                }
            }
        }
        Ok(output)
    }
}

pub fn current_thread_cpu_100ns() -> Result<u64> {
    use windows::Win32::{
        Foundation::FILETIME,
        System::Threading::{GetCurrentThread, GetThreadTimes},
    };
    let mut created = FILETIME::default();
    let mut exited = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: The pseudo-handle is this thread and all FILETIME outputs remain live.
    unsafe {
        GetThreadTimes(
            GetCurrentThread(),
            &mut created,
            &mut exited,
            &mut kernel,
            &mut user,
        )?;
    }
    Ok(filetime_ticks(kernel) + filetime_ticks(user))
}

pub struct ProcessExit {
    handle: OwnedHandle,
}
impl ProcessExit {
    pub fn open(pid: u32) -> Result<Self> {
        use windows::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE};
        // SAFETY: The caller supplies a discovered test-owned PID. This handle grants wait access only.
        let raw = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid)? };
        Ok(Self {
            // SAFETY: OpenProcess returned this new owned handle; transfer ownership exactly once.
            handle: unsafe { OwnedHandle::from_raw_handle(raw.0) },
        })
    }
    pub fn wait(&self, timeout: std::time::Duration) -> Result<()> {
        use windows::Win32::{Foundation::WAIT_OBJECT_0, System::Threading::WaitForSingleObject};
        // SAFETY: The owned wait handle remains valid throughout the wait.
        let status = unsafe {
            WaitForSingleObject(
                HANDLE(self.handle.as_raw_handle()),
                timeout.as_millis() as u32,
            )
        };
        ensure!(
            status == WAIT_OBJECT_0,
            "Owned helper process exit was not confirmed within {} ms.",
            timeout.as_millis()
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn modified_codec_is_rejected_without_execution() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("not-a-codec.exe");
        std::fs::write(&path, b"not executable").unwrap();
        assert!(verified_file(&path, &"0".repeat(64)).is_err());
    }
    #[test]
    fn verified_codec_lock_prevents_replacement_and_mutation() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("fixture.bin");
        std::fs::write(&path, b"source-owned fixture").unwrap();
        let hash = format!("{:x}", Sha256::digest(b"source-owned fixture"));
        let verified = verified_file(&path, &hash).unwrap();
        assert!(std::fs::write(&path, b"changed").is_err());
        assert!(std::fs::rename(&path, temporary.path().join("moved.bin")).is_err());
        drop(verified);
        std::fs::write(&path, b"changed").unwrap();
        assert!(verified_file(&path, &hash).is_err());
    }
}
