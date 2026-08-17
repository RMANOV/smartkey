use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn git(repo: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|value| value.trim().to_owned())
}

fn override_value(name: &str) -> Option<String> {
    match env::var(name) {
        Ok(value) => Some(value),
        Err(env::VarError::NotPresent) => None,
        Err(env::VarError::NotUnicode(_)) => panic!("{name} must be valid UTF-8"),
    }
}

fn valid_sha(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn watch_git_path(repo: &Path, name: &str) {
    if let Some(value) = git(repo, &["rev-parse", "--git-path", name]) {
        let path = PathBuf::from(value);
        let absolute = if path.is_absolute() {
            path
        } else {
            repo.join(path)
        };
        println!("cargo:rerun-if-changed={}", absolute.display());
    }
}

fn main() {
    println!("cargo:rerun-if-env-changed=SMARTKEY_BUILD_GIT_SHA");
    println!("cargo:rerun-if-env-changed=SMARTKEY_BUILD_GIT_DIRTY");

    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let repo = manifest.join("../..");
    watch_git_path(&repo, "HEAD");
    watch_git_path(&repo, "index");
    if let Some(head_ref) = git(&repo, &["symbolic-ref", "-q", "HEAD"]) {
        watch_git_path(&repo, &head_ref);
    }

    let sha_override = override_value("SMARTKEY_BUILD_GIT_SHA");
    let dirty_override = override_value("SMARTKEY_BUILD_GIT_DIRTY");
    let (sha, dirty) = match (sha_override, dirty_override) {
        (Some(sha), Some(dirty)) => {
            assert!(
                valid_sha(&sha),
                "SMARTKEY_BUILD_GIT_SHA must be 40 lowercase hexadecimal characters, got {sha:?}"
            );
            assert!(
                dirty == "0" || dirty == "1",
                "SMARTKEY_BUILD_GIT_DIRTY must be exactly 0 or 1, got {dirty:?}"
            );
            (sha, dirty)
        }
        (None, None) => {
            let sha = git(&repo, &["rev-parse", "--verify", "HEAD"])
                .filter(|value| valid_sha(value))
                .unwrap_or_else(|| "unknown".to_owned());
            // Cargo cannot reliably invalidate this build-script result for
            // every unstaged or untracked worktree change. Only a build
            // wrapper that supplies both overrides may attest clean/dirty.
            (sha, "unknown".to_owned())
        }
        _ => {
            panic!("SMARTKEY_BUILD_GIT_SHA and SMARTKEY_BUILD_GIT_DIRTY must be supplied together")
        }
    };

    println!("cargo:rustc-env=SMARTKEY_BUILD_GIT_SHA={sha}");
    println!("cargo:rustc-env=SMARTKEY_BUILD_GIT_DIRTY={dirty}");
}
