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
    "Status-SmartKey.cmd",
    "Enable-SmartKey.cmd",
    "Disable-SmartKey.cmd",
    "Uninstall-SmartKey.cmd",
    "README-WINDOWS.md",
    "LICENSE",
    "LICENSE-GPL",
    "LICENSE-APACHE",
];

/// Validate the entire distribution before touching the destination. Existing
/// configuration, public corpus customizations and personal files are retained.
pub fn stage(bundle: &Path, install: &Path, data: &Path) -> io::Result<PathBuf> {
    for name in BINARIES.iter().chain(DATA).chain(SUPPORT) {
        let path = bundle.join(name);
        if !path.is_file() || fs::metadata(&path)?.len() == 0 {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("Incomplete SmartKey bundle: {name}. Extract the whole archive first."),
            ));
        }
    }
    fs::create_dir_all(install)?;
    fs::create_dir_all(data)?;
    for name in BINARIES.iter().chain(SUPPORT).chain(DATA) {
        let from = bundle.join(name);
        let to = install.join(name);
        // Running the installed helper again must not overwrite itself.
        if from.canonicalize()? != to.canonicalize().unwrap_or_else(|_| to.clone()) {
            fs::copy(from, to)?;
        }
    }
    for name in DATA {
        let to = data.join(name);
        if !to.exists() {
            // create_new prevents racing another installer or user editor.
            let mut destination = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(to)?;
            let mut source = fs::File::open(bundle.join(name))?;
            io::copy(&mut source, &mut destination)?;
        }
    }
    Ok(install.join("smartkey-register.exe"))
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
    fn stages_binaries_and_missing_data_but_preserves_user_files() {
        let s = Scratch::new();
        let bundle = s.bundle();
        let install = s.0.join("Local/SmartKey");
        let data = s.0.join("Roaming/smartkey");
        fs::create_dir_all(&data).unwrap();
        fs::write(data.join("smartkey.json"), "existing user config").unwrap();
        fs::write(data.join("personal.json"), "existing personal data").unwrap();
        fs::write(data.join("corpus_en.json"), "custom corpus").unwrap();
        assert_eq!(
            stage(&bundle, &install, &data).unwrap(),
            install.join("smartkey-register.exe")
        );
        assert_eq!(
            fs::read(data.join("smartkey.json")).unwrap(),
            b"existing user config"
        );
        assert_eq!(
            fs::read(data.join("personal.json")).unwrap(),
            b"existing personal data"
        );
        assert_eq!(
            fs::read(data.join("corpus_en.json")).unwrap(),
            b"custom corpus"
        );
        assert_eq!(
            fs::read(install.join("smartkey_win.dll")).unwrap(),
            fs::read(bundle.join("smartkey_win.dll")).unwrap()
        );
        assert!(data.join("corpus_bg.json").is_file());
        // Reinstall is allowed from both distribution and installed location.
        stage(&bundle, &install, &data).unwrap();
        stage(&install, &install, &data).unwrap();
    }
    #[test]
    fn missing_payload_refuses_before_destination_changes() {
        let s = Scratch::new();
        let bundle = s.bundle();
        fs::remove_file(bundle.join("smartkey_win.dll")).unwrap();
        let install = s.0.join("install");
        let data = s.0.join("data");
        assert!(stage(&bundle, &install, &data).is_err());
        assert!(!install.exists() && !data.exists());
    }
}
