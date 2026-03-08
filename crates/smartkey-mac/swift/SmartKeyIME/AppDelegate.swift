// SmartKey macOS IME — App delegate that starts the IMKServer.
//
// The .app bundle runs as a background process (LSBackgroundOnly = true).
// macOS discovers it as an input method via the Info.plist configuration.

import Cocoa
import InputMethodKit

@main
class AppDelegate: NSObject, NSApplicationDelegate {
    var server: IMKServer!

    func applicationDidFinishLaunching(_ notification: Notification) {
        // The connection name must match Info.plist's InputMethodConnectionName.
        server = IMKServer(
            name: "SmartKey_Connection",
            bundleIdentifier: Bundle.main.bundleIdentifier!
        )
    }
}
