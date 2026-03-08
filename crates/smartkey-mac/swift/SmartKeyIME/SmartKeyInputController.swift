// SmartKey macOS Input Method — IMKInputController subclass.
//
// This is the thin Swift adapter that bridges macOS Input Method Kit
// to the Rust InputMethodCore via C FFI.
//
// Build: Link against libsmartkey_mac.dylib

import Cocoa
import InputMethodKit

// C FFI declarations (from smartkey-mac Rust cdylib)
@_silgen_name("smartkey_new")
func smartkey_new(_ config: UnsafePointer<CChar>?) -> OpaquePointer

@_silgen_name("smartkey_free")
func smartkey_free(_ handle: OpaquePointer)

@_silgen_name("smartkey_handle_key")
func smartkey_handle_key(_ handle: OpaquePointer, _ keycode: UInt32, _ modifiers: UInt32) -> UnsafeMutablePointer<CActionList>

@_silgen_name("smartkey_focus_lost")
func smartkey_focus_lost(_ handle: OpaquePointer) -> UnsafeMutablePointer<CActionList>

@_silgen_name("smartkey_focus_gained")
func smartkey_focus_gained(_ handle: OpaquePointer)

@_silgen_name("smartkey_reset")
func smartkey_reset(_ handle: OpaquePointer) -> UnsafeMutablePointer<CActionList>

@_silgen_name("smartkey_free_actions")
func smartkey_free_actions(_ list: UnsafeMutablePointer<CActionList>)

@_silgen_name("smartkey_free_string")
func smartkey_free_string(_ s: UnsafeMutablePointer<CChar>)

// Mirror the C structs
struct CAction {
    var action_type: UInt32  // 0=ShowGhost, 1=HideGhost, 2=CommitText, 3=ForwardKey
    var payload: UnsafeMutablePointer<CChar>?
}

struct CActionList {
    var actions: UnsafeMutablePointer<CAction>
    var count: UInt32
}

class SmartKeyInputController: IMKInputController {
    private var core: OpaquePointer!

    override init!(server: IMKServer!, delegate: Any!, client inputClient: Any!) {
        super.init(server: server, delegate: delegate, client: inputClient)
        core = smartkey_new(nil)
    }

    deinit {
        if core != nil {
            smartkey_free(core)
        }
    }

    override func handle(_ event: NSEvent!, client sender: Any!) -> Bool {
        guard let event = event, event.type == .keyDown else {
            return false
        }

        let keycode = UInt32(event.keyCode)
        let modifiers = UInt32(event.modifierFlags.rawValue)

        let actionList = smartkey_handle_key(core, keycode, modifiers)
        defer { smartkey_free_actions(actionList) }

        let client = sender as! IMKTextInput
        return executeActions(actionList, client: client)
    }

    override func activateServer(_ sender: Any!) {
        smartkey_focus_gained(core)
    }

    override func deactivateServer(_ sender: Any!) {
        let actions = smartkey_focus_lost(core)
        if let client = sender as? IMKTextInput {
            executeActions(actions, client: client)
        }
        smartkey_free_actions(actions)
    }

    @discardableResult
    private func executeActions(_ list: UnsafeMutablePointer<CActionList>, client: IMKTextInput) -> Bool {
        var consumed = true
        let count = Int(list.pointee.count)

        for i in 0..<count {
            let action = list.pointee.actions[i]

            switch action.action_type {
            case 0: // ShowGhost
                if let payload = action.payload {
                    let text = String(cString: payload)
                    // Display as marked (inline) text with grey styling.
                    let attrs: [NSAttributedString.Key: Any] = [
                        .foregroundColor: NSColor.systemGray
                    ]
                    client.setMarkedText(
                        NSAttributedString(string: text, attributes: attrs),
                        selectionRange: NSRange(location: 0, length: 0),
                        replacementRange: NSRange(location: NSNotFound, length: 0)
                    )
                }

            case 1: // HideGhost
                client.setMarkedText(
                    "",
                    selectionRange: NSRange(location: 0, length: 0),
                    replacementRange: NSRange(location: NSNotFound, length: 0)
                )

            case 2: // CommitText
                if let payload = action.payload {
                    let text = String(cString: payload)
                    client.insertText(text, replacementRange: NSRange(location: NSNotFound, length: 0))
                }

            case 3: // ForwardKey
                consumed = false

            default:
                break
            }
        }

        return consumed
    }
}
