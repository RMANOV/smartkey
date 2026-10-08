//! Filesystem-only staging, shared by the installer and portable tests.
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const BINARIES: &[&str] = &["smartkey-register.exe", "smartkey_win.dll"];
pub const DATA: &[&str] = &[
    "smartkey.json",
    "corpus_en.json",
    "corpus_bg.json",
    "corpus_tech.json",
];
pub const SUPPORT: &[&str] = &[
    "Install-SmartKey.cmd",
    "Install-Machine-SmartKey.cmd",
    "Uninstall-Machine-SmartKey.cmd",
    "windows-runtime-manifest.json",
    "artifact-source-manifest.json",
    "Status-SmartKey.cmd",
    "Enable-SmartKey.cmd",
    "Disable-SmartKey.cmd",
    "Uninstall-SmartKey.cmd",
    "README-WINDOWS.md",
    "LICENSE",
    "LICENSE-GPL",
    "LICENSE-APACHE",
];

/// Validate every required payload before touching any destination.
pub fn validate_bundle(bundle: &Path) -> io::Result<()> {
    for name in BINARIES.iter().chain(DATA).chain(SUPPORT) {
        let path = bundle.join(name);
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Incomplete or redirected SmartKey bundle: {name}. Extract the whole archive first."),
            ));
        }
    }
    Ok(())
}

/// Machine phase: destination must be new, with a protected parent validated
/// by the Windows caller. It never creates or reads any user's data folder.
pub fn stage_machine(bundle: &Path, install: &Path) -> io::Result<PathBuf> {
    validate_bundle(bundle)?;
    create_machine_directory(install)?;
    for name in BINARIES.iter().chain(DATA).chain(SUPPORT) {
        let mut source = fs::File::open(bundle.join(name))?;
        let mut destination = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(install.join(name))?;
        set_machine_file_owner(&install.join(name))?;
        io::copy(&mut source, &mut destination)?;
    }
    Ok(install.join("smartkey-register.exe"))
}

#[cfg(not(windows))]
fn set_machine_file_owner(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(windows)]
fn set_machine_file_owner(path: &Path) -> io::Result<()> {
    use windows::core::*;
    use windows::Win32::Security::Authorization::{SetNamedSecurityInfoW, SE_FILE_OBJECT};
    use windows::Win32::Security::{
        CreateWellKnownSid, WinBuiltinAdministratorsSid, OWNER_SECURITY_INFORMATION, PSID,
    };
    let mut storage = [0u32; 17];
    let sid = PSID(storage.as_mut_ptr().cast());
    let mut size = std::mem::size_of_val(&storage) as u32;
    unsafe { CreateWellKnownSid(WinBuiltinAdministratorsSid, PSID::default(), sid, &mut size) }
        .map_err(io::Error::other)?;
    let name: Vec<u16> = path
        .as_os_str()
        .to_string_lossy()
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let status = unsafe {
        SetNamedSecurityInfoW(
            PCWSTR(name.as_ptr()),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            sid,
            PSID::default(),
            None,
            None,
        )
    };
    if status.0 != 0 {
        return Err(io::Error::other(Error::from(HRESULT::from_win32(status.0))));
    }
    Ok(())
}

#[cfg(not(windows))]
fn create_machine_directory(path: &Path) -> io::Result<()> {
    fs::create_dir(path)
}

#[cfg(windows)]
fn create_machine_directory(path: &Path) -> io::Result<()> {
    use windows::core::*;
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
    use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
    use windows::Win32::Storage::FileSystem::CreateDirectoryW;
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    unsafe {
        // Administrators own the directory. Only SYSTEM/Administrators can
        // write; users can read/execute. Children inherit the same grants.
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            w!("O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FRFX;;;BU)"),
            1,
            &mut descriptor,
            None,
        )
    }
    .map_err(io::Error::other)?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: false.into(),
    };
    let name: Vec<u16> = path
        .as_os_str()
        .to_string_lossy()
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let result = unsafe { CreateDirectoryW(PCWSTR(name.as_ptr()), Some(&attributes)) };
    unsafe {
        LocalFree(HLOCAL(descriptor.0));
    }
    result.map_err(io::Error::other)
}

/// Normal-user phase: copy only missing starter data from the verified machine
/// bundle. It never stages DLLs/EXEs and preserves existing personal files.
pub fn stage_user_data(bundle: &Path, data: &Path) -> io::Result<()> {
    validate_bundle(bundle)?;
    fs::create_dir_all(data)?;
    for name in DATA {
        let to = data.join(name);
        if !to.exists() {
            let mut destination = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(to)?;
            let mut source = fs::File::open(bundle.join(name))?;
            io::copy(&mut source, &mut destination)?;
        }
    }
    Ok(())
}

/// Remove only allowlisted files in the exact verified machine directory.
/// Refuse unknown objects before deleting anything; never recursive-delete.
pub fn remove_machine_files(install: &Path) -> io::Result<()> {
    if !install.exists() {
        return Ok(());
    }
    let expected: Vec<_> = BINARIES
        .iter()
        .chain(DATA)
        .chain(SUPPORT)
        .copied()
        .collect();
    for entry in fs::read_dir(install)? {
        let entry = entry?;
        if !entry.file_type()?.is_file()
            || !expected
                .iter()
                .any(|name| entry.file_name() == std::ffi::OsStr::new(name))
        {
            return Err(io::Error::other(
                "Unexpected object in SmartKey machine directory; it was preserved",
            ));
        }
    }
    for name in expected {
        let path = install.join(name);
        if path.exists() {
            fs::remove_file(path)?;
        }
    }
    fs::remove_dir(install)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static SEQUENCE: AtomicUsize = AtomicUsize::new(0);
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "smartkey-stage-{}-{}",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&p).unwrap();
            Self(p)
        }
        fn bundle(&self) -> PathBuf {
            let p = self.0.join("bundle with spaces");
            fs::create_dir(&p).unwrap();
            for name in BINARIES.iter().chain(DATA).chain(SUPPORT) {
                fs::write(p.join(name), format!("public bundle {name}")).unwrap();
            }
            p
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }
    #[test]
    fn machine_stage_does_not_create_user_data_and_refuses_existing_destination() {
        let s = Scratch::new();
        let bundle = s.bundle();
        let install = s.0.join("ProgramFiles-SmartKey");
        let personal = s.0.join("Roaming-smartkey");
        stage_machine(&bundle, &install).unwrap();
        assert!(!personal.exists());
        fs::write(install.join("smartkey_win.dll"), "existing machine binary").unwrap();
        assert!(stage_machine(&bundle, &install).is_err());
        assert_eq!(
            fs::read(install.join("smartkey_win.dll")).unwrap(),
            b"existing machine binary"
        );
    }

    #[test]
    fn user_stage_preserves_personal_data_and_never_stages_binaries() {
        let s = Scratch::new();
        let bundle = s.bundle();
        let data = s.0.join("Roaming-smartkey");
        fs::create_dir(&data).unwrap();
        fs::write(data.join("smartkey.json"), "existing config").unwrap();
        fs::write(data.join("personal.json"), "private sentinel").unwrap();
        fs::write(data.join("corpus_en.json"), "custom corpus").unwrap();
        stage_user_data(&bundle, &data).unwrap();
        stage_user_data(&bundle, &data).unwrap();
        assert_eq!(
            fs::read(data.join("smartkey.json")).unwrap(),
            b"existing config"
        );
        assert_eq!(
            fs::read(data.join("personal.json")).unwrap(),
            b"private sentinel"
        );
        assert_eq!(
            fs::read(data.join("corpus_en.json")).unwrap(),
            b"custom corpus"
        );
        assert!(data.join("corpus_bg.json").is_file());
        assert!(!data.join("smartkey_win.dll").exists());
        assert!(!data.join("smartkey-register.exe").exists());
    }

    #[test]
    fn incomplete_bundle_refuses_machine_and_user_destinations() {
        let s = Scratch::new();
        let bundle = s.bundle();
        fs::remove_file(bundle.join("smartkey_win.dll")).unwrap();
        let install = s.0.join("install");
        let data = s.0.join("data");
        assert!(stage_machine(&bundle, &install).is_err());
        assert!(stage_user_data(&bundle, &data).is_err());
        assert!(!install.exists());
        assert!(!data.exists());
    }

    #[test]
    fn machine_removal_refuses_unknown_objects_before_deleting_known_files() {
        let s = Scratch::new();
        let bundle = s.bundle();
        let install = s.0.join("install");
        stage_machine(&bundle, &install).unwrap();
        fs::write(install.join("unexpected.txt"), "preserved").unwrap();
        assert!(remove_machine_files(&install).is_err());
        assert!(install.join("smartkey_win.dll").is_file());
        fs::remove_file(install.join("unexpected.txt")).unwrap();
        remove_machine_files(&install).unwrap();
        assert!(!install.exists());
    }
}
