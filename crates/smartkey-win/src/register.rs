// COM/TSF registration helper for SmartKey Windows IME.
//
// Usage:
//   smartkey-register.exe --install    Register as a TSF Text Input Processor
//   smartkey-register.exe --uninstall  Remove registration

fn main() {
    let args: Vec<String> = std::env::args().collect();

    match args.get(1).map(|s| s.as_str()) {
        Some("--install") => {
            println!("SmartKey IME registration");
            println!("CLSID: {{{}}}", smartkey_win::config::CLSID_SMARTKEY_STR);

            #[cfg(windows)]
            {
                let dll_path = find_dll_path();
                println!("DLL: {dll_path}");

                init_com();
                match smartkey_win::registration::register(&dll_path) {
                    Ok(system_wide) => {
                        if system_wide {
                            println!("SmartKey IME registered system-wide (HKLM).");
                        } else {
                            println!("SmartKey IME registered per-user (HKCU).");
                        }
                    }
                    Err(e) => {
                        log::error!("Registration failed: {e}");
                        uninit_com();
                        std::process::exit(1);
                    }
                }
                uninit_com();
            }

            #[cfg(not(windows))]
            println!("(dry run — not on Windows)");
        }
        Some("--uninstall") => {
            println!("Unregistering SmartKey IME...");

            #[cfg(windows)]
            {
                init_com();
                match smartkey_win::registration::unregister() {
                    Ok(()) => println!("SmartKey IME unregistered successfully."),
                    Err(e) => {
                        log::error!("Unregistration failed: {e}");
                        uninit_com();
                        std::process::exit(1);
                    }
                }
                uninit_com();
            }

            #[cfg(not(windows))]
            println!("(dry run — not on Windows)");
        }
        _ => {
            log::error!("Usage: smartkey-register [--install | --uninstall]");
            std::process::exit(1);
        }
    }
}

/// Find the DLL in the same directory as this executable.
#[cfg(windows)]
fn find_dll_path() -> String {
    let exe = std::env::current_exe().expect("cannot determine exe path");
    let dir = exe.parent().expect("exe has no parent directory");
    dir.join("smartkey_win.dll").to_string_lossy().into_owned()
}

#[cfg(windows)]
fn init_com() {
    let hr = unsafe {
        windows::Win32::System::Com::CoInitializeEx(
            None,
            windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
        )
    };
    if hr.is_err() {
        log::error!("COM initialization failed: {hr:?}");
        std::process::exit(1);
    }
}

#[cfg(windows)]
fn uninit_com() {
    unsafe {
        windows::Win32::System::Com::CoUninitialize();
    }
}
