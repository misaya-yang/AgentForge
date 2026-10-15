import AppKit
import ApplicationServices
import CoreGraphics
import CryptoKit
import Foundation
import Security

// This helper is compiled into the packaged companion. It is deliberately a
// one-request CLI, not a daemon or listener. The Python broker supplies one
// JSON object on stdin and receives one JSON object on stdout.

enum HelperFailure: Error {
    case denied(String, String)
}

func emit(_ value: [String: Any], status: Int32 = 0) -> Never {
    let data = try! JSONSerialization.data(withJSONObject: value, options: [.sortedKeys])
    FileHandle.standardOutput.write(data)
    FileHandle.standardOutput.write(Data([0x0A]))
    exit(status)
}

func fail(_ code: String, _ message: String) -> Never {
    emit(["ok": false, "code": code, "message": message], status: 2)
}

func requestObject() -> [String: Any] {
    let data = FileHandle.standardInput.readDataToEndOfFile()
    guard data.count > 0 && data.count <= 1_048_576 else {
        fail("invalid_request", "request must contain at most one MiB of JSON")
    }
    guard let object = try? JSONSerialization.jsonObject(with: data),
          let request = object as? [String: Any] else {
        fail("invalid_request", "request must be one JSON object")
    }
    return request
}

func requiredString(_ object: [String: Any], _ key: String, max: Int = 4096) -> String {
    guard let value = object[key] as? String,
          !value.isEmpty,
          value.utf8.count <= max,
          !value.unicodeScalars.contains(where: { $0.value < 0x20 }) else {
        fail("invalid_request", "\(key) is invalid")
    }
    return value
}

func optionalString(_ object: [String: Any], _ key: String, max: Int = 4096) -> String? {
    guard let raw = object[key], !(raw is NSNull) else { return nil }
    guard let value = raw as? String,
          !value.isEmpty,
          value.utf8.count <= max,
          !value.unicodeScalars.contains(where: { $0.value < 0x20 }) else {
        fail("invalid_request", "\(key) is invalid")
    }
    return value
}

func number(_ object: [String: Any], _ key: String) -> Double? {
    guard let raw = object[key] as? NSNumber else { return nil }
    return raw.doubleValue
}

func axAttribute(_ element: AXUIElement, _ attribute: CFString) -> CFTypeRef? {
    var value: CFTypeRef?
    guard AXUIElementCopyAttributeValue(element, attribute, &value) == .success else {
        return nil
    }
    return value
}

func axString(_ element: AXUIElement, _ attribute: CFString) -> String? {
    return axAttribute(element, attribute) as? String
}

func axElements(_ element: AXUIElement, _ attribute: CFString) -> [AXUIElement] {
    return axAttribute(element, attribute) as? [AXUIElement] ?? []
}

func axPoint(_ element: AXUIElement, _ attribute: CFString) -> CGPoint? {
    guard let raw = axAttribute(element, attribute), CFGetTypeID(raw) == AXValueGetTypeID() else {
        return nil
    }
    let value = unsafeBitCast(raw, to: AXValue.self)
    var point = CGPoint.zero
    guard AXValueGetType(value) == .cgPoint,
          AXValueGetValue(value, .cgPoint, &point) else { return nil }
    return point
}

func axSize(_ element: AXUIElement, _ attribute: CFString) -> CGSize? {
    guard let raw = axAttribute(element, attribute), CFGetTypeID(raw) == AXValueGetTypeID() else {
        return nil
    }
    let value = unsafeBitCast(raw, to: AXValue.self)
    var size = CGSize.zero
    guard AXValueGetType(value) == .cgSize,
          AXValueGetValue(value, .cgSize, &size) else { return nil }
    return size
}

struct AXScan {
    var text: [String]
    var secureField = false
    var dialog = false
    var mfa = false
    var password = false
    var permission = false
    var origin: String?
}

let mfaMarkers = [
    "verification code", "two-factor", "two factor", "2fa", "one-time code",
    "one time code", "authenticator", "passcode", "验证码", "两步验证"
]
let passwordMarkers = ["password", "passphrase", "pin code", "密码", "口令"]
let permissionMarkers = [
    "accessibility", "screen recording", "screen capture", "allow access",
    "grant access", "system settings", "privacy & security", "权限", "隐私与安全"
]

func normalizedOrigin(_ candidate: String) -> String? {
    guard let components = URLComponents(string: candidate),
          let scheme = components.scheme?.lowercased() else { return nil }
    if scheme == "file" { return "file://" }
    guard ["http", "https"].contains(scheme), let host = components.host?.lowercased() else {
        return nil
    }
    var value = "\(scheme)://\(host)"
    if let port = components.port { value += ":\(port)" }
    return value
}

func scanAccessibility(_ window: AXUIElement) -> AXScan {
    var scan = AXScan(text: [])
    var queue: [(AXUIElement, Int)] = [(window, 0)]
    var index = 0
    var textBytes = 0
    while index < queue.count && index < 800 {
        let (element, depth) = queue[index]
        index += 1
        let role = axString(element, kAXRoleAttribute as CFString) ?? ""
        let subrole = axString(element, kAXSubroleAttribute as CFString) ?? ""
        let identifier = axString(element, kAXIdentifierAttribute as CFString) ?? ""
        let secure = subrole == (kAXSecureTextFieldSubrole as String)
        let isDialog = role == (kAXSheetRole as String)
            || subrole == (kAXDialogSubrole as String)
            || subrole == (kAXSystemDialogSubrole as String)
            || subrole == "AXApplicationAlertDialog"
            || subrole == "AXApplicationDialog"
        scan.secureField = scan.secureField || secure
        scan.dialog = scan.dialog || isDialog

        let title = axString(element, kAXTitleAttribute as CFString)
        let description = axString(element, kAXDescriptionAttribute as CFString)
        let rawValue = secure ? nil : axString(element, kAXValueAttribute as CFString)
        let urlValue = axString(element, kAXURLAttribute as CFString)
        let candidates = [title, description, rawValue]
            .compactMap { $0 }
            .map { String($0.prefix(500)) }
            .filter { !$0.isEmpty }
        let combined = candidates.joined(separator: " ").lowercased()
        if mfaMarkers.contains(where: { combined.contains($0) }) { scan.mfa = true }
        if passwordMarkers.contains(where: { combined.contains($0) }) { scan.password = true }
        if permissionMarkers.contains(where: { combined.contains($0) }) && isDialog {
            scan.permission = true
        }
        if secure { scan.password = true }

        if scan.origin == nil {
            for candidate in [urlValue, rawValue].compactMap({ $0 }) {
                if let origin = normalizedOrigin(candidate) {
                    let descriptor = ((description ?? "") + " " + identifier).lowercased()
                    if urlValue != nil || descriptor.contains("address") || role == "AXWebArea" {
                        scan.origin = origin
                        break
                    }
                }
            }
        }

        var parts = [role, subrole, identifier].filter { !$0.isEmpty }
        if secure {
            parts.append("[SECURE_FIELD_REDACTED]")
        } else {
            parts.append(contentsOf: candidates)
        }
        if let position = axPoint(element, kAXPositionAttribute as CFString),
           let size = axSize(element, kAXSizeAttribute as CFString) {
            parts.append(
                String(
                    format: "frame=%.1f,%.1f,%.1f,%.1f",
                    position.x,
                    position.y,
                    size.width,
                    size.height
                )
            )
        }
        let line = parts.joined(separator: " | ")
        if !line.isEmpty && textBytes + line.utf8.count <= 64 * 1024 {
            scan.text.append(line)
            textBytes += line.utf8.count
        }
        if depth < 12 {
            queue.append(contentsOf: axElements(element, kAXChildrenAttribute as CFString).map {
                ($0, depth + 1)
            })
        }
    }
    return scan
}

struct WindowSnapshot {
    let app: NSRunningApplication
    let window: AXUIElement
    let windowID: CGWindowID
    let bounds: CGRect
    let title: String
    let frontmost: Bool
    let scan: AXScan
}

func focusedWindow(for bundleID: String) throws -> WindowSnapshot {
    guard AXIsProcessTrusted() else {
        throw HelperFailure.denied("accessibility_denied", "Accessibility permission is not ready")
    }
    let applications = NSRunningApplication.runningApplications(withBundleIdentifier: bundleID)
    guard let app = applications.first(where: { !$0.isTerminated }) else {
        throw HelperFailure.denied("app_not_running", "the allowed application is not running")
    }
    let applicationElement = AXUIElementCreateApplication(app.processIdentifier)
    guard let rawWindow = axAttribute(applicationElement, kAXFocusedWindowAttribute as CFString),
          CFGetTypeID(rawWindow) == AXUIElementGetTypeID() else {
        throw HelperFailure.denied("window_unavailable", "the application has no focused window")
    }
    let window = unsafeBitCast(rawWindow, to: AXUIElement.self)
    let position = axPoint(window, kAXPositionAttribute as CFString)
    let size = axSize(window, kAXSizeAttribute as CFString)
    let axBounds = position.flatMap { point in size.map { CGRect(origin: point, size: $0) } }
    let windowTitle = axString(window, kAXTitleAttribute as CFString) ?? ""

    let options: CGWindowListOption = [.optionOnScreenOnly, .excludeDesktopElements]
    guard let list = CGWindowListCopyWindowInfo(options, kCGNullWindowID) as? [[String: Any]] else {
        throw HelperFailure.denied("window_unavailable", "window server metadata is unavailable")
    }
    let owned = list.filter { item in
        (item[kCGWindowOwnerPID as String] as? NSNumber)?.int32Value == app.processIdentifier
            && (item[kCGWindowLayer as String] as? NSNumber)?.intValue == 0
    }
    func bounds(_ item: [String: Any]) -> CGRect? {
        guard let raw = item[kCGWindowBounds as String] as? NSDictionary else { return nil }
        return CGRect(dictionaryRepresentation: raw)
    }
    let selected = owned.min { left, right in
        guard let expected = axBounds,
              let leftBounds = bounds(left),
              let rightBounds = bounds(right) else { return false }
        let leftDelta = abs(leftBounds.origin.x - expected.origin.x)
            + abs(leftBounds.origin.y - expected.origin.y)
            + abs(leftBounds.width - expected.width)
            + abs(leftBounds.height - expected.height)
        let rightDelta = abs(rightBounds.origin.x - expected.origin.x)
            + abs(rightBounds.origin.y - expected.origin.y)
            + abs(rightBounds.width - expected.width)
            + abs(rightBounds.height - expected.height)
        return leftDelta < rightDelta
    }
    guard let item = selected,
          let windowNumber = item[kCGWindowNumber as String] as? NSNumber,
          let cgBounds = bounds(item) else {
        throw HelperFailure.denied("window_unavailable", "focused window is not on screen")
    }
    let frontmost = NSWorkspace.shared.frontmostApplication?.bundleIdentifier == bundleID
    return WindowSnapshot(
        app: app,
        window: window,
        windowID: CGWindowID(windowNumber.uint32Value),
        bounds: cgBounds,
        title: windowTitle,
        frontmost: frontmost,
        scan: scanAccessibility(window)
    )
}

func snapshotObject(_ snapshot: WindowSnapshot) -> [String: Any] {
    var flags: [String] = []
    if snapshot.scan.secureField { flags.append("secure_text_field") }
    if snapshot.scan.password { flags.append("password_flow") }
    if snapshot.scan.mfa { flags.append("mfa_flow") }
    if snapshot.scan.dialog { flags.append("security_or_modal_dialog") }
    if snapshot.scan.permission { flags.append("permission_dialog") }
    return [
        "ok": true,
        "app_id": snapshot.app.bundleIdentifier ?? "",
        "pid": snapshot.app.processIdentifier,
        "window_id": String(snapshot.windowID),
        "window_title": snapshot.title,
        "x": snapshot.bounds.origin.x,
        "y": snapshot.bounds.origin.y,
        "width": snapshot.bounds.width,
        "height": snapshot.bounds.height,
        "frontmost": snapshot.frontmost,
        "origin": snapshot.scan.origin ?? NSNull(),
        "accessibility_lines": snapshot.scan.text,
        "risk_flags": flags,
    ]
}

func requireBoundSnapshot(_ request: [String: Any]) throws -> WindowSnapshot {
    let appID = requiredString(request, "app_id", max: 255)
    let expectedWindow = requiredString(request, "window_id", max: 32)
    let expectedOrigin = optionalString(request, "expected_origin", max: 2048)
    let snapshot = try focusedWindow(for: appID)
    guard String(snapshot.windowID) == expectedWindow else {
        throw HelperFailure.denied("stale_window", "focused window identity changed")
    }
    if snapshot.scan.origin != expectedOrigin {
        throw HelperFailure.denied("stale_origin", "browser origin changed")
    }
    if snapshot.scan.secureField || snapshot.scan.password || snapshot.scan.mfa
        || snapshot.scan.dialog || snapshot.scan.permission {
        throw HelperFailure.denied(
            "takeover_required",
            "password, MFA, permission, or security dialog requires trusted user takeover"
        )
    }
    return snapshot
}

func mouseEvent(_ type: CGEventType, point: CGPoint, clickCount: Int64 = 1) throws {
    guard let event = CGEvent(
        mouseEventSource: nil,
        mouseType: type,
        mouseCursorPosition: point,
        mouseButton: .left
    ) else { throw HelperFailure.denied("input_unavailable", "cannot create mouse event") }
    event.setIntegerValueField(.mouseEventClickState, value: clickCount)
    event.post(tap: .cghidEventTap)
}

func click(_ point: CGPoint, count: Int64) throws {
    try mouseEvent(.leftMouseDown, point: point, clickCount: count)
    usleep(25_000)
    try mouseEvent(.leftMouseUp, point: point, clickCount: count)
}

func typeText(_ text: String, delayMicros: useconds_t) throws {
    let units = Array(text.utf16)
    var offset = 0
    while offset < units.count {
        let end = min(offset + 20, units.count)
        var chunk = Array(units[offset..<end])
        guard let down = CGEvent(keyboardEventSource: nil, virtualKey: 0, keyDown: true),
              let up = CGEvent(keyboardEventSource: nil, virtualKey: 0, keyDown: false) else {
            throw HelperFailure.denied("input_unavailable", "cannot create keyboard event")
        }
        chunk.withUnsafeMutableBufferPointer { buffer in
            down.keyboardSetUnicodeString(stringLength: buffer.count, unicodeString: buffer.baseAddress!)
            up.keyboardSetUnicodeString(stringLength: buffer.count, unicodeString: buffer.baseAddress!)
        }
        down.post(tap: .cghidEventTap)
        up.post(tap: .cghidEventTap)
        offset = end
        if delayMicros > 0 { usleep(delayMicros) }
    }
}

let keyCodes: [String: CGKeyCode] = [
    "a": 0, "s": 1, "c": 8, "v": 9, "x": 7, "z": 6,
    "return": 36, "enter": 36, "tab": 48, "space": 49,
    "backspace": 51, "delete": 51, "escape": 53,
    "left": 123, "right": 124, "down": 125, "up": 126,
]

func pressKey(_ specification: String) throws {
    let parts = specification.lowercased().split(separator: "+").map(String.init)
    guard let keyName = parts.last, let code = keyCodes[keyName] else {
        throw HelperFailure.denied("unsupported_key", "key is outside the bounded native map")
    }
    var flags: CGEventFlags = []
    for modifier in parts.dropLast() {
        switch modifier {
        case "cmd", "command", "meta": flags.insert(.maskCommand)
        case "shift": flags.insert(.maskShift)
        case "ctrl", "control": flags.insert(.maskControl)
        case "alt", "option": flags.insert(.maskAlternate)
        default: throw HelperFailure.denied("unsupported_key", "modifier is unsupported")
        }
    }
    guard let down = CGEvent(keyboardEventSource: nil, virtualKey: code, keyDown: true),
          let up = CGEvent(keyboardEventSource: nil, virtualKey: code, keyDown: false) else {
        throw HelperFailure.denied("input_unavailable", "cannot create keyboard event")
    }
    down.flags = flags
    up.flags = flags
    down.post(tap: .cghidEventTap)
    usleep(20_000)
    up.post(tap: .cghidEventTap)
}

func activate(_ application: NSRunningApplication) throws {
    guard application.activate(options: [.activateAllWindows]) else {
        throw HelperFailure.denied("focus_failed", "the allowed application could not be focused")
    }
    let bundleID = application.bundleIdentifier
    let deadline = Date().addingTimeInterval(2)
    while Date() < deadline {
        if NSWorkspace.shared.frontmostApplication?.bundleIdentifier == bundleID { return }
        usleep(20_000)
    }
    throw HelperFailure.denied("focus_failed", "the allowed application did not become frontmost")
}

func openURL(_ raw: String, with application: NSRunningApplication) throws {
    guard let url = URL(string: raw),
          ["file", "http", "https"].contains(url.scheme?.lowercased() ?? ""),
          let applicationURL = application.bundleURL else {
        throw HelperFailure.denied("url_denied", "URL is invalid or outside allowed schemes")
    }
    let semaphore = DispatchSemaphore(value: 0)
    var failure: Error?
    let configuration = NSWorkspace.OpenConfiguration()
    configuration.activates = true
    NSWorkspace.shared.open(
        [url],
        withApplicationAt: applicationURL,
        configuration: configuration
    ) { _, error in
        failure = error
        semaphore.signal()
    }
    if semaphore.wait(timeout: .now() + 5) == .timedOut {
        throw HelperFailure.denied("open_failed", "opening the allowed URL timed out")
    }
    if failure != nil {
        throw HelperFailure.denied("open_failed", "the allowed URL could not be opened")
    }
}

func runAction(_ request: [String: Any]) throws -> [String: Any] {
    let kind = requiredString(request, "kind", max: 40)
    let arguments = request["arguments"] as? [String: Any] ?? [:]
    let snapshot = try requireBoundSnapshot(request)
    if kind != "focus_app" && kind != "open_app" && !snapshot.frontmost {
        throw HelperFailure.denied("target_not_frontmost", "input target is not frontmost")
    }
    let localPoint: (String, String) -> CGPoint = { xName, yName in
        guard let x = number(arguments, xName), let y = number(arguments, yName),
              x >= 0, y >= 0, x < snapshot.bounds.width, y < snapshot.bounds.height else {
            fail("invalid_request", "coordinates are outside the bound window")
        }
        return CGPoint(x: snapshot.bounds.origin.x + x, y: snapshot.bounds.origin.y + y)
    }
    switch kind {
    case "focus_app":
        try activate(snapshot.app)
    case "open_app":
        if let url = optionalString(arguments, "url", max: 8192) {
            try openURL(url, with: snapshot.app)
        } else {
            try activate(snapshot.app)
        }
    case "click":
        try click(localPoint("x", "y"), count: 1)
    case "double_click":
        let point = localPoint("x", "y")
        try click(point, count: 1)
        usleep(80_000)
        try click(point, count: 2)
    case "type_text":
        guard let text = arguments["text"] as? String, text.utf8.count <= 10_000 else {
            throw HelperFailure.denied("invalid_request", "text exceeds the bounded input size")
        }
        let delay = max(0, min(number(arguments, "delay_ms") ?? 2, 100))
        try typeText(text, delayMicros: useconds_t(delay * 1000))
    case "key_press":
        try pressKey(requiredString(arguments, "key", max: 100))
    case "scroll":
        let point: CGPoint
        if number(arguments, "x") != nil || number(arguments, "y") != nil {
            point = localPoint("x", "y")
        } else {
            point = CGPoint(x: snapshot.bounds.midX, y: snapshot.bounds.midY)
        }
        CGWarpMouseCursorPosition(point)
        guard let deltaNumber = arguments["scroll_y"] as? NSNumber,
              let event = CGEvent(
                scrollWheelEvent2Source: nil,
                units: .pixel,
                wheelCount: 1,
                wheel1: Int32(clamping: deltaNumber.intValue),
                wheel2: 0,
                wheel3: 0
              ) else {
            throw HelperFailure.denied("invalid_request", "scroll delta is invalid")
        }
        event.post(tap: .cghidEventTap)
    case "drag":
        let from = localPoint("from_x", "from_y")
        let to = localPoint("to_x", "to_y")
        try mouseEvent(.leftMouseDown, point: from)
        for step in 1...10 {
            let ratio = CGFloat(step) / 10
            let point = CGPoint(
                x: from.x + (to.x - from.x) * ratio,
                y: from.y + (to.y - from.y) * ratio
            )
            try mouseEvent(.leftMouseDragged, point: point)
            usleep(10_000)
        }
        try mouseEvent(.leftMouseUp, point: to)
    case "wait":
        let duration = max(1, min(number(arguments, "duration_ms") ?? 100, 10_000))
        usleep(useconds_t(duration * 1000))
    default:
        throw HelperFailure.denied("unsupported_action", "action primitive is unsupported")
    }
    return ["ok": true, "kind": kind]
}

func stopInput() {
    if let mouseUp = CGEvent(
        mouseEventSource: nil,
        mouseType: .leftMouseUp,
        mouseCursorPosition: CGEvent(source: nil)?.location ?? .zero,
        mouseButton: .left
    ) {
        mouseUp.post(tap: .cghidEventTap)
    }
    for code: CGKeyCode in [54, 55, 56, 57, 58, 59, 60, 61, 62] {
        CGEvent(keyboardEventSource: nil, virtualKey: code, keyDown: false)?.post(
            tap: .cghidEventTap
        )
    }
}

func runApprovalPrompt(_ request: [String: Any]) -> [String: Any] {
    let title = requiredString(request, "title", max: 160)
    let summary = requiredString(request, "summary", max: 4000)
    let timeout = max(5, min(number(request, "timeout_seconds") ?? 60, 300))
    NSApplication.shared.setActivationPolicy(.accessory)
    NSApplication.shared.activate(ignoringOtherApps: true)
    let timeoutFlag = AtomicFlag()
    let alert = NSAlert()
    alert.alertStyle = .warning
    alert.messageText = title
    alert.informativeText = summary
    alert.addButton(withTitle: "Approve Once")
    alert.addButton(withTitle: "Take Over / Deny")
    DispatchQueue.global(qos: .userInitiated).asyncAfter(deadline: .now() + timeout) {
        timeoutFlag.set()
        DispatchQueue.main.async {
            NSApplication.shared.abortModal()
        }
    }
    let response = alert.runModal()
    let timedOut = timeoutFlag.value
    return [
        "ok": true,
        "approved": !timedOut && response == .alertFirstButtonReturn,
        "timed_out": timedOut,
    ]
}

final class AtomicFlag: @unchecked Sendable {
    private let lock = NSLock()
    private var stored = false

    func set() {
        lock.lock()
        stored = true
        lock.unlock()
    }

    var value: Bool {
        lock.lock()
        defer { lock.unlock() }
        return stored
    }
}

func approvalKey(service: String, account: String, create: Bool) throws -> Data {
    let query: [String: Any] = [
        kSecClass as String: kSecClassGenericPassword,
        kSecAttrService as String: service,
        kSecAttrAccount as String: account,
        kSecReturnData as String: true,
        kSecMatchLimit as String: kSecMatchLimitOne,
    ]
    var item: CFTypeRef?
    let status = SecItemCopyMatching(query as CFDictionary, &item)
    if status == errSecSuccess, let data = item as? Data, data.count == 32 {
        return data
    }
    if status != errSecItemNotFound || !create {
        throw HelperFailure.denied(
            "approval_key_unavailable",
            "trusted-local Keychain approval key is unavailable"
        )
    }
    var bytes = [UInt8](repeating: 0, count: 32)
    guard SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes) == errSecSuccess else {
        throw HelperFailure.denied(
            "approval_key_unavailable",
            "trusted-local Keychain approval key could not be generated"
        )
    }
    let data = Data(bytes)
    let insert: [String: Any] = [
        kSecClass as String: kSecClassGenericPassword,
        kSecAttrService as String: service,
        kSecAttrAccount as String: account,
        kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
        kSecValueData as String: data,
    ]
    guard SecItemAdd(insert as CFDictionary, nil) == errSecSuccess else {
        throw HelperFailure.denied(
            "approval_key_unavailable",
            "trusted-local Keychain approval key could not be stored"
        )
    }
    return data
}

func approvalHMAC(key: Data, payload: Data) -> String {
    let digest = HMAC<SHA256>.authenticationCode(
        for: payload,
        using: SymmetricKey(data: key)
    )
    return Data(digest).map { String(format: "%02x", $0) }.joined()
}

func approvalSign(_ request: [String: Any]) throws -> [String: Any] {
    let prompt = runApprovalPrompt(request)
    guard prompt["approved"] as? Bool == true else {
        return prompt.merging(["signature": NSNull()]) { current, _ in current }
    }
    let service = requiredString(request, "keychain_service", max: 255)
    let account = requiredString(request, "keychain_account", max: 255)
    let encoded = requiredString(request, "approval_payload_base64", max: 1_400_000)
    guard let payload = Data(base64Encoded: encoded),
          !payload.isEmpty,
          payload.count <= 1_048_576 else {
        throw HelperFailure.denied("invalid_request", "approval payload is invalid")
    }
    let key = try approvalKey(service: service, account: account, create: true)
    return prompt.merging(["signature": approvalHMAC(key: key, payload: payload)]) {
        current, _ in current
    }
}

func approvalVerify(_ request: [String: Any]) throws -> [String: Any] {
    let service = requiredString(request, "keychain_service", max: 255)
    let account = requiredString(request, "keychain_account", max: 255)
    let encoded = requiredString(request, "approval_payload_base64", max: 1_400_000)
    let signature = requiredString(request, "signature", max: 128)
    guard let payload = Data(base64Encoded: encoded),
          !payload.isEmpty,
          payload.count <= 1_048_576 else {
        throw HelperFailure.denied("invalid_request", "approval payload is invalid")
    }
    let key = try approvalKey(service: service, account: account, create: false)
    let expected = approvalHMAC(key: key, payload: payload)
    let verified = expected.utf8.count == signature.utf8.count
        && zip(expected.utf8, signature.utf8).reduce(UInt8(0)) {
            $0 | ($1.0 ^ $1.1)
        } == 0
    return ["ok": true, "verified": verified]
}

let request = requestObject()
let command = requiredString(request, "command", max: 40)
do {
    switch command {
    case "doctor":
        emit([
            "ok": true,
            "accessibility": AXIsProcessTrusted(),
            "screen_recording": CGPreflightScreenCaptureAccess(),
        ])
    case "observe":
        let snapshot = try focusedWindow(for: requiredString(request, "app_id", max: 255))
        if let expectedWindow = optionalString(request, "window_id", max: 32),
           expectedWindow != String(snapshot.windowID) {
            throw HelperFailure.denied("stale_window", "focused window identity changed")
        }
        emit(snapshotObject(snapshot))
    case "action":
        emit(try runAction(request))
    case "stop":
        stopInput()
        emit(["ok": true, "stopped": true])
    case "approval_prompt":
        emit(runApprovalPrompt(request))
    case "approval_sign":
        emit(try approvalSign(request))
    case "approval_verify":
        emit(try approvalVerify(request))
    default:
        fail("unsupported_command", "helper command is unsupported")
    }
} catch let HelperFailure.denied(code, message) {
    fail(code, message)
} catch {
    fail("native_backend_failed", "native helper failed without returning sensitive details")
}
