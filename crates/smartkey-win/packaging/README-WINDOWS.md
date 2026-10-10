# SmartKey for Windows 10/11 x64

This archive contains the complete application and public starter data. No Git,
Rust, Python or Visual C++ installer is required. Extract the whole archive;
keep it available for machine removal. Source/native acceptance status is
recorded separately in the supplied manifests; actual typing still needs a
Windows desktop check.

## Install once on a permitted computer

If an older SmartKey Windows version is installed, remove that version before
installing this shared machine package. If this package is already installed and
user setup reports an old per-user COM conflict, ask an administrator to review
that exact registration. Do not blindly run the legacy uninstaller at that point:
it may remove the shared TSF catalog. Setup preserves the conflict rather than
deleting an unknown registration.

An administrator is needed once to register the shared Windows text service.
Right-click **Install-Machine-SmartKey.cmd** in the extracted archive and choose
**Run as administrator**. It copies the complete bundle to the Windows Program
Files known folder, in **SmartKey**, and registers the machine COM/TSF catalog.
This phase does not enable SmartKey for the administrator, edit their AppData,
change their keyboards, or add a login entry. It refuses an existing machine
installation; remove it explicitly before replacing it.

Then close the administrator window. As the person who will type, double-click
**Install-SmartKey.cmd normally**. This enables SmartKey for that account, copies
only missing starter data to that user's **AppData\Roaming\smartkey**, verifies
it in a separate process, and adds one user login entry and Start menu commands.
Existing configuration, public corpus changes and personal files are preserved.
If machine installation is missing, this step stops with instructions before
changing user data or keyboard settings. It never triggers automatic elevation.
Each additional permitted user runs this normal step in their own account.

## Use and return to your ordinary keyboard

Press **Win+Space** and choose SmartKey. Press **Win+Space** again to select your
ordinary keyboard. Existing layouts and your default choice remain available.
Open a new text app after installation. SmartKey is available at the user's next
Windows login; it does not force itself to become the default keyboard.

Start menu > SmartKey provides **Status**, **Enable**, **Disable** and **Uninstall**.
Disable removes this user's SmartKey login entry. Enable restores availability
and login. Run these commands normally, without administrator elevation.

## Remove

**Uninstall-SmartKey.cmd** removes only the current user's availability, login
entry and menu commands. It preserves personal data and the shared machine files.

When no one needs the shared installation, close apps using SmartKey. Right-click
**Uninstall-Machine-SmartKey.cmd in the original extracted archive** and choose
**Run as administrator**. It removes the exact shared catalog/COM registration and
allowlisted Program Files bundle. Run it from the archive, not the installed
Program Files directory, so the running helper can delete every machine file.
Other keyboards and all users' personal data are preserved. Machine removal does
not access other users' profiles; each configured user can remove their own login
entry using Uninstall-SmartKey.cmd from the archive. Locked files or failed
registration operations produce an error and can be retried; no success is claimed
for a partial removal.

## Files and privacy

The archive contains EXE/DLL, seven visible CMD commands, two manifests, public
JSON starter data, this README and license notices. It never includes personal.json.
User data stays in the user's AppData; no network connection is needed for setup
or typing. No service, scheduled task or PowerShell policy bypass is installed.
