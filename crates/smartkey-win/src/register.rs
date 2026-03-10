// COM/TSF registration helper for SmartKey Windows IME.
//
// Usage:
//   smartkey-register.exe --install    Register as a TSF Text Input Processor
//   smartkey-register.exe --uninstall  Remove registration
//
// Registers:
//   - CLSID in HKLM\SOFTWARE\Classes\CLSID\{CLSID_SMARTKEY}
//   - TIP profile in HKLM\SOFTWARE\Microsoft\CTF\TIP\{CLSID_SMARTKEY}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    match args.get(1).map(|s| s.as_str()) {
        Some("--install") => {
            println!("SmartKey IME registration");
            println!("CLSID: {{{}}}", smartkey_win::config::CLSID_SMARTKEY);
            #[cfg(windows)]
            {
                // TODO: Write CLSID to HKLM\SOFTWARE\Classes\CLSID\{...}
                // TODO: Register TIP via ITfInputProcessorProfileMgr::RegisterProfile
                // TODO: Set display name and icon path
                eprintln!("WARNING: Registration not yet implemented.");
                std::process::exit(1);
            }
            #[cfg(not(windows))]
            {
                println!("(dry run — not on Windows)");
            }
        }
        Some("--uninstall") => {
            println!("Unregistering SmartKey IME...");
            #[cfg(windows)]
            {
                // TODO: Remove CLSID and TIP registration
                eprintln!("WARNING: Registration not yet implemented.");
                std::process::exit(1);
            }
            #[cfg(not(windows))]
            {
                println!("(dry run — not on Windows)");
            }
        }
        _ => {
            eprintln!("Usage: smartkey-register [--install | --uninstall]");
            std::process::exit(1);
        }
    }
}
