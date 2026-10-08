# SmartKey for Windows

This folder contains the Windows application and public English/Bulgarian dictionaries. You do not need Git, Python, Rust or a repository checkout. Use the x64 bundle on Windows 10/11 x64 on a machine where you are allowed to install software.

Extract the entire ZIP, then double-click **Install-SmartKey.cmd**. The visible result window reports success or the exact failed operation. Installation uses your account and does not request administrator elevation. If Windows or workplace policy refuses installation, the error remains visible; ask your administrator rather than bypassing that policy.

The installer copies the program to `%LOCALAPPDATA%\SmartKey`, and copies missing public dictionaries and starter configuration to `%APPDATA%\smartkey`. It preserves existing configuration, customized dictionaries and personal data. Keep the whole ZIP for repair or installation on another permitted machine.

After installation, press **Win+Space** and choose **SmartKey**. Press **Win+Space** again to return to your normal keyboard whenever you want. Installation does not replace your existing layouts or change your default keyboard. Open a new text application if an application already running does not show the new choice.

SmartKey availability is enabled at Windows login through one current-user login entry. Start menu → **SmartKey** has:

- **Status-SmartKey** — checks the installed profile and binary location.
- **Disable-SmartKey** — disables SmartKey and removes its login entry; your ordinary keyboards and personal data remain.
- **Enable-SmartKey** — enables SmartKey and its login entry again; select it with Win+Space.
- **Uninstall-SmartKey** — removes SmartKey registration and its login entry, then deletes the installed EXE/DLL. Personal data remains in `%APPDATA%\smartkey`.

Close applications using SmartKey before updating or uninstalling. If a file is still in use, removal reports the remaining file instead of claiming that it was deleted. After registration is removed you may delete the remaining `%LOCALAPPDATA%\SmartKey` support folder. Personal data is kept separately.

No telemetry, network download, scheduled task or Windows service is installed. This package's build/registration checks do not replace validation of actual typing in your target applications.
