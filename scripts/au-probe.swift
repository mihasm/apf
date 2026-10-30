// au-probe — looks at an installed Audio Unit the way Logic does (through the v2 API, also
// for AUv3s, which macOS bridges), without Logic and without opening any audio device.
//
//   swiftc -O scripts/au-probe.swift -o au-probe
//   ./au-probe aumu ApD3 MiSm            parameters: order, groups, units, value names
//   ./au-probe aumu ApD3 MiSm --gui 30   also opens the web view for 30 s and prints every
//                                        gesture/value event the host receives meanwhile
//   ./au-probe aumu ApD3 MiSm --preset 2 selects factory preset 2 as a host does, then
//                                        prints the parameters again
//   ./au-probe aumu ApT3 MiSm --gui 10 --resize [--in-process]
//                                        also resizes the web view's window a few times, as a
//                                        user dragging its corner, and checks that the page
//                                        covers the window each time (the window is red
//                                        behind it; a capture must show no red);
//                                        --in-process: an AUv2 in the host's process with
//                                        its own Cocoa view, as Logic hosts it
//   ./au-probe aumu ApMo MiSm --gui 3 --keys [--in-process]
//                                        also types into the web view (A tapped, A held,
//                                        Shift) and checks that every key the page leaves
//                                        reaches the host whole, key-down, key-up and
//                                        modifier changes, as Logic's Musical Typing needs
//
// AUv3s are loaded out of process, as Logic loads them.

import AppKit
import AudioToolbox
import AVFoundation
import CoreAudioKit

func fourCC(_ s: String) -> OSType {
    s.utf8.reduce(0) { ($0 << 8) | OSType($1) }
}

func paramName(_ au: AudioUnit, _ id: AudioUnitParameterID) -> String {
    var info = AudioUnitParameterInfo()
    var size = UInt32(MemoryLayout<AudioUnitParameterInfo>.size)
    guard AudioUnitGetProperty(au, kAudioUnitProperty_ParameterInfo, kAudioUnitScope_Global, id, &info, &size) == noErr else { return "?" }
    if let cf = info.cfNameString { return cf.takeUnretainedValue() as String }
    return withUnsafeBytes(of: info.name) { String(cString: $0.bindMemory(to: CChar.self).baseAddress!) }
}

let args = CommandLine.arguments
guard args.count >= 4 else {
    print("usage: au-probe <type> <subtype> <manufacturer> [--gui SECONDS]")
    exit(2)
}
var guiSeconds: Double? = nil
if let i = args.firstIndex(of: "--gui"), i + 1 < args.count { guiSeconds = Double(args[i + 1]) }
let resizeTest = args.contains("--resize")
let keysTest = args.contains("--keys")
var presetNumber: Int? = nil
if let i = args.firstIndex(of: "--preset"), i + 1 < args.count { presetNumber = Int(args[i + 1]) }

let desc = AudioComponentDescription(componentType: fourCC(args[1]), componentSubType: fourCC(args[2]),
                                     componentManufacturer: fourCC(args[3]), componentFlags: 0, componentFlagsMask: 0)
let app = NSApplication.shared
app.setActivationPolicy(.regular)

var avUnit: AVAudioUnit?
// AUv3s out of process, as every host loads them; AUv2s with --in-process as Logic loads them.
let inProcess = args.contains("--in-process")
AVAudioUnit.instantiate(with: desc, options: inProcess ? [] : [.loadOutOfProcess]) { unit, error in
    if let error { print("instantiate failed: \(error)"); exit(1) }
    avUnit = unit
}
while avUnit == nil { RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.05)) }
let unit = avUnit!
let au = unit.audioUnit
print("loaded:     \(unit.name) — \(unit.manufacturerName), version \(unit.version)")

func printValues(_ title: String) {
    var size: UInt32 = 0
    AudioUnitGetPropertyInfo(au, kAudioUnitProperty_ParameterList, kAudioUnitScope_Global, 0, &size, nil)
    var ids = [AudioUnitParameterID](repeating: 0, count: Int(size) / MemoryLayout<AudioUnitParameterID>.size)
    AudioUnitGetProperty(au, kAudioUnitProperty_ParameterList, kAudioUnitScope_Global, 0, &ids, &size)
    print(title)
    for id in ids {
        var value: AudioUnitParameterValue = 0
        AudioUnitGetParameter(au, id, kAudioUnitScope_Global, 0, &value)
        var text = ""
        if let p = unit.auAudioUnit.parameterTree?.parameter(withAddress: AUParameterAddress(id)) {
            text = p.string(fromValue: nil)
        }
        print(String(format: "  %-12@ %g  %@", paramName(au, id) as NSString, value, text as NSString))
    }
}

// Factory presets, through both APIs (Logic reads the v3 list for an AUv3).
let v3Presets = unit.auAudioUnit.factoryPresets ?? []
print("presets:    \(v3Presets.count) factory (v3): " + v3Presets.map { "\($0.number) \($0.name)" }.joined(separator: ", "))
var presetsRef: Unmanaged<CFArray>?
var psize = UInt32(MemoryLayout<Unmanaged<CFArray>?>.size)
if AudioUnitGetProperty(au, kAudioUnitProperty_FactoryPresets, kAudioUnitScope_Global, 0, &presetsRef, &psize) == noErr,
   let arr = presetsRef?.takeUnretainedValue() {
    let n = CFArrayGetCount(arr)
    var names: [String] = []
    for i in 0..<n {
        let p = CFArrayGetValueAtIndex(arr, i)!.assumingMemoryBound(to: AUPreset.self).pointee
        names.append("\(p.presetNumber) \(p.presetName?.takeUnretainedValue() as String? ?? "?")")
    }
    print("presets:    \(n) factory (v2): " + names.joined(separator: ", "))
} else {
    print("presets:    none through the v2 API")
}
print("current:    \(unit.auAudioUnit.currentPreset.map { "\($0.number) \($0.name)" } ?? "none")")

// Parameters as a v2 host sees them.
var size: UInt32 = 0
AudioUnitGetPropertyInfo(au, kAudioUnitProperty_ParameterList, kAudioUnitScope_Global, 0, &size, nil)
var ids = [AudioUnitParameterID](repeating: 0, count: Int(size) / MemoryLayout<AudioUnitParameterID>.size)
AudioUnitGetProperty(au, kAudioUnitProperty_ParameterList, kAudioUnitScope_Global, 0, &ids, &size)
print("parameters: \(ids.count) (in the order the host lists them)")
for id in ids {
    var info = AudioUnitParameterInfo()
    var isz = UInt32(MemoryLayout<AudioUnitParameterInfo>.size)
    AudioUnitGetProperty(au, kAudioUnitProperty_ParameterInfo, kAudioUnitScope_Global, id, &info, &isz)
    var clump = ""
    if info.flags.contains(.flag_HasClump) {
        var cn = AudioUnitParameterNameInfo(inID: info.clumpID, inDesiredLength: Int32(kAudioUnitParameterName_Full), outName: nil)
        var csz = UInt32(MemoryLayout<AudioUnitParameterNameInfo>.size)
        if AudioUnitGetProperty(au, kAudioUnitProperty_ParameterClumpName, kAudioUnitScope_Global, 0, &cn, &csz) == noErr,
           let n = cn.outName { clump = " [group \(info.clumpID) \(n.takeRetainedValue() as String)]" }
        else { clump = " [group \(info.clumpID)]" }
    }
    var strings = ""
    var vsz: UInt32 = 0
    if AudioUnitGetPropertyInfo(au, kAudioUnitProperty_ParameterValueStrings, kAudioUnitScope_Global, id, &vsz, nil) == noErr {
        var arr: Unmanaged<CFArray>?
        vsz = UInt32(MemoryLayout<Unmanaged<CFArray>?>.size)
        if AudioUnitGetProperty(au, kAudioUnitProperty_ParameterValueStrings, kAudioUnitScope_Global, id, &arr, &vsz) == noErr,
           let a = arr?.takeRetainedValue() as? [String] { strings = " names \(a)" }
    }
    var value: AudioUnitParameterValue = 0
    AudioUnitGetParameter(au, id, kAudioUnitScope_Global, 0, &value)
    print(String(format: "  %10u  %@ unit %u  %g .. %g  value %g", id, paramName(au, id) as NSString,
                 info.unit.rawValue, info.minValue, info.maxValue, value) + clump + strings)
}

// The AUv3 view of the same tree: group order as the plugin declares it.
if let tree = unit.auAudioUnit.parameterTree {
    func walk(_ node: AUParameterNode, _ depth: Int) {
        if let g = node as? AUParameterGroup {
            if depth > 0 { print(String(repeating: "  ", count: depth) + "group \(g.displayName)") }
            g.children.forEach { walk($0, depth + 1) }
        } else if let p = node as? AUParameter {
            print(String(repeating: "  ", count: depth) + "\(p.displayName)" + (p.valueStrings.map { " \($0)" } ?? ""))
        }
    }
    print("tree:")
    walk(tree, 0)
}

if let n = presetNumber {
    guard let preset = v3Presets.first(where: { $0.number == n }) else { print("no factory preset \(n)"); exit(1) }
    unit.auAudioUnit.currentPreset = preset
    RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.3))
    print("selected:   \(preset.number) \(preset.name); current now \(unit.auAudioUnit.currentPreset?.name ?? "none")")
    printValues("values:")
}

guard let seconds = guiSeconds else { exit(0) }

// Every event a v2 host (Logic) would get while the web view is used.
var listener: AUEventListenerRef?
AUEventListenerCreate({ _, _, event, _, value in
    let e = event.pointee
    let id = e.mArgument.mParameter.mParameterID
    let name = paramName(e.mArgument.mParameter.mAudioUnit, id)
    switch e.mEventType {
    case .beginParameterChangeGesture: print("event:      begin gesture  \(name)")
    case .endParameterChangeGesture: print("event:      end gesture    \(name)")
    case .parameterValueChange: print("event:      value          \(name) = \(value)")
    default: break
    }
}, nil, CFRunLoopGetMain(), CFRunLoopMode.defaultMode.rawValue, 0.005, 0.005, &listener)
for id in ids {
    for type in [AudioUnitEventType.beginParameterChangeGesture, .endParameterChangeGesture, .parameterValueChange] {
        var ev = AudioUnitEvent(mEventType: type, mArgument: AudioUnitEvent.__Unnamed_union_mArgument(
            mParameter: AudioUnitParameter(mAudioUnit: au, mParameterID: id, mScope: kAudioUnitScope_Global, mElement: 0)))
        AUEventListenerAddEventType(listener!, nil, &ev)
    }
}

/// Share of pure red pixels in a capture of the window: the part of the window the web view
/// does not cover.
func redShare(_ window: NSWindow) -> Double? {
    let file = NSTemporaryDirectory() + "au-probe-\(getpid()).png"
    let p = Process()
    p.executableURL = URL(fileURLWithPath: "/usr/sbin/screencapture")
    p.arguments = ["-x", "-o", "-l\(window.windowNumber)", file]
    try? p.run()
    p.waitUntilExit()
    // The last capture stays for a look: <tmp>/au-probe-last.png.
    defer {
        let last = NSTemporaryDirectory() + "au-probe-last.png"
        try? FileManager.default.removeItem(atPath: last)
        try? FileManager.default.moveItem(atPath: file, toPath: last)
    }
    guard let data = FileManager.default.contents(atPath: file), let rep = NSBitmapImageRep(data: data) else { return nil }
    var red = 0, total = 0
    // The content area only: skip the title bar (top 28 points).
    let top = Int(28 * Double(rep.pixelsHigh) / window.frame.height)
    for y in stride(from: top, to: rep.pixelsHigh, by: 4) {
        for x in stride(from: 0, to: rep.pixelsWide, by: 4) {
            guard let c = rep.colorAt(x: x, y: y)?.usingColorSpace(.sRGB) else { continue }
            total += 1
            // The window's red after colour matching is about (0.92, 0.20, 0.14).
            if c.redComponent > 0.8 && c.greenComponent < 0.35 && c.blueComponent < 0.3 { red += 1 }
        }
    }
    return total > 0 ? Double(red) / Double(total) : nil
}

var viewWindow: NSWindow?
var viewController: NSViewController?

/// Key events that came back past the web view to the host's view: ("down" | "up" | "flags", key code).
var hostKeys: [(String, UInt16)] = []

/// The host's view around the web view: a key the web view leaves goes up the responder chain to
/// it, as to Logic's key handling (key commands, Musical Typing).
class HostKeyView: NSView {
    override func keyDown(with event: NSEvent) { hostKeys.append(("down", event.keyCode)) }
    override func keyUp(with event: NSEvent) { hostKeys.append(("up", event.keyCode)) }
    override func flagsChanged(with event: NSEvent) { hostKeys.append(("flags", event.keyCode)) }
}

/// An AUv2's own Cocoa view, as Logic gets it (`kAudioUnitProperty_CocoaUI`).
func cocoaView(_ au: AudioUnit) -> NSView? {
    var size: UInt32 = 0
    var writable: DarwinBoolean = false
    guard AudioUnitGetPropertyInfo(au, kAudioUnitProperty_CocoaUI, kAudioUnitScope_Global, 0, &size, &writable) == noErr, size > 0 else { return nil }
    let info = UnsafeMutableRawPointer.allocate(byteCount: Int(size), alignment: 8)
    defer { info.deallocate() }
    guard AudioUnitGetProperty(au, kAudioUnitProperty_CocoaUI, kAudioUnitScope_Global, 0, info, &size) == noErr else { return nil }
    let v = info.load(as: AudioUnitCocoaViewInfo.self)
    let url = v.mCocoaAUViewBundleLocation.takeUnretainedValue() as URL
    guard let bundle = Bundle(url: url), let name = v.mCocoaAUViewClass.takeUnretainedValue() as String?,
          let cls = bundle.classNamed(name) as? NSObject.Type else { return nil }
    let factory = cls.init()
    let sel = NSSelectorFromString("uiViewForAudioUnit:withSize:")
    typealias Fn = @convention(c) (AnyObject, Selector, AudioUnit, NSSize) -> Unmanaged<NSView>?
    let fn = unsafeBitCast(factory.method(for: sel), to: Fn.self)
    return fn(factory, sel, au, NSSize(width: 0, height: 0))?.takeUnretainedValue()
}

let requestView: (@escaping (NSViewController?) -> Void) -> Void = { done in
    if inProcess, let view = cocoaView(au) {
        let vc = NSViewController()
        vc.view = view
        done(vc)
    } else {
        unit.auAudioUnit.requestViewController(completionHandler: done)
    }
}
requestView { vc in
    guard let vc else { print("no web view"); exit(1) }
    let size = vc.preferredContentSize.width > 0 ? vc.preferredContentSize
        : vc.view.frame.width > 0 ? vc.view.frame.size : NSSize(width: 560, height: 340)
    var style: NSWindow.StyleMask = [.titled, .closable]
    if resizeTest { style.insert(.resizable) }
    let window = NSWindow(contentRect: NSRect(origin: NSPoint(x: 300, y: 300), size: size),
                          styleMask: style, backing: .buffered, defer: false)
    window.title = "au-probe: \(unit.name)"
    window.backgroundColor = resizeTest ? .red : window.backgroundColor
    if resizeTest || keysTest {
        // As a host embeds a web view: its view inside the host's own, following its size.
        let container: NSView = keysTest ? HostKeyView(frame: NSRect(origin: .zero, size: size)) : NSView(frame: NSRect(origin: .zero, size: size))
        vc.view.frame = container.bounds
        vc.view.autoresizingMask = [.width, .height]
        container.addSubview(vc.view)
        window.contentView = container
        viewController = vc
    } else {
        window.contentViewController = vc
    }
    window.setContentSize(size)
    // AU_PROBE_QUIET=1: show the window without taking the keyboard from the app in use.
    if ProcessInfo.processInfo.environment["AU_PROBE_QUIET"] == "1" {
        window.orderFrontRegardless()
        // Key of this process only (its own events go to it); the app in use keeps the keyboard.
        if keysTest { window.makeKey() }
    } else {
        window.makeKeyAndOrderFront(nil)
        app.activate(ignoringOtherApps: true)
    }
    viewWindow = window
    print("gui:        open for \(Int(seconds)) s, \(Int(size.width)) x \(Int(size.height))")
}
// Quick, all within ~2 s of opening: a window WindowServer reports occluded (behind a locked
// screen; on 2026-10-24 every new window of a bare executable on the dev Mac) gets its page
// suspended by WebKit ~3 s after it loads, and an out-of-process web view cannot be told
// otherwise from here. AU_PROBE_SETTLE / AU_PROBE_STEP (seconds) change the timing.
let settle = Double(ProcessInfo.processInfo.environment["AU_PROBE_SETTLE"] ?? "") ?? 0.6
let step = Double(ProcessInfo.processInfo.environment["AU_PROBE_STEP"] ?? "") ?? 0.25
if resizeTest {
    RunLoop.main.run(until: Date(timeIntervalSinceNow: settle))
    guard let window = viewWindow else { print("resize:     no web view window"); exit(1) }
    print("resize:     window limits \(window.contentMinSize) .. \(window.contentMaxSize), web view constraints \(viewController?.view.constraints.count ?? 0)")
    // macOS pins the web view at its preferred size with priority 501: a user's drag (510)
    // wins, a program's setFrame (500) does not. Resize as the drag would.
    for c in viewController?.view.constraints ?? [] where c.identifier?.hasPrefix("NSViewController.preferredContentSize") == true {
        c.priority = .init(499)
    }
    var failed = false
    let opened = redShare(window) ?? -1
    print(String(format: "resize:     as opened: %.1f %% not covered by the web view", opened * 100))
    if opened < 0 || opened > 0.01 { failed = true }
    for (w, h) in [(1280.0, 800.0), (300.0, 200.0), (900.0, 600.0), (1100.0, 700.0)] {
        // Not setContentSize: a web view may pin the content size with constraints
        // (AUv3 remote views do); a user's drag resizes the frame.
        let frame = window.frameRect(forContentRect: NSRect(origin: window.frame.origin, size: NSSize(width: w, height: h)))
        window.setFrame(frame, display: true)
        RunLoop.main.run(until: Date(timeIntervalSinceNow: step))
        let got = window.contentLayoutRect.size
        let uncovered = redShare(window) ?? -1
        // Below the web view's minimum the page keeps its size: red there is expected.
        let sized = abs(got.width - w) < 1 && abs(got.height - h) < 1
        let ok = sized && uncovered >= 0 && (uncovered < 0.01 || w < 500)
        if !ok { failed = true }
        print(String(format: "resize:     window %.0f x %.0f (content %.0f x %.0f): %.1f %% not covered by the web view%@", w, h, got.width, got.height, uncovered * 100, ok ? "" : "  FAILED"))
    }
    print(failed ? "resize:     FAILED" : "resize:     OK, the web view follows the window")
}
if keysTest {
    RunLoop.main.run(until: Date(timeIntervalSinceNow: settle + 1))
    guard let window = viewWindow, let view = viewController?.view else { print("keys:       no web view window"); exit(1) }
    // The keyboard focus into the web view, as a click would put it: an AUv2's webview, an
    // AUv3's remote view (out of process).
    func find(_ v: NSView) -> NSView? {
        let name = String(describing: type(of: v))
        if name.contains("WebView") || name.contains("RemoteView") { return v }
        return v.subviews.lazy.compactMap(find).first
    }
    let focus = find(view) ?? view
    window.makeFirstResponder(focus)
    print("keys:       focus in \(type(of: focus))")
    func key(_ type: NSEvent.EventType, _ chars: String, _ code: UInt16, _ flags: NSEvent.ModifierFlags = []) {
        let e = NSEvent.keyEvent(with: type, location: .zero, modifierFlags: flags, timestamp: ProcessInfo.processInfo.systemUptime,
                                 windowNumber: window.windowNumber, context: nil, characters: chars,
                                 charactersIgnoringModifiers: chars, isARepeat: false, keyCode: code)!
        app.postEvent(e, atStart: false)
    }
    // The run loop alone hands no queued events to windows: dispatch them as an app's loop does.
    func pump(_ t: Double) {
        let until = Date(timeIntervalSinceNow: t)
        while Date() < until {
            if let e = app.nextEvent(matching: .any, until: Date(timeIntervalSinceNow: 0.02), inMode: .default, dequeue: true) { app.sendEvent(e) }
        }
    }
    func wait(_ n: Int) {
        let until = Date(timeIntervalSinceNow: 3)
        while hostKeys.count < n && Date() < until { pump(0.02) }
    }
    key(.keyDown, "a", 0); key(.keyUp, "a", 0)
    wait(2)
    key(.keyDown, "a", 0)
    pump(0.3)
    key(.keyUp, "a", 0)
    wait(4)
    key(.flagsChanged, "", 56, .shift); key(.flagsChanged, "", 56)
    wait(6)
    pump(0.2)
    let a = hostKeys.filter { $0.1 == 0 && $0.0 != "flags" }.map { $0.0 }
    let flags = hostKeys.filter { $0.0 == "flags" }.count
    print("keys:       reached the host: A tapped and held \(a.joined(separator: " ")), modifier changes \(flags)")
    let ok = a == ["down", "up", "down", "up"] && flags == 2
    print(ok ? "keys:       OK" : "keys:       FAILED")
    if !ok { exit(1) }
}
RunLoop.main.run(until: Date(timeIntervalSinceNow: seconds))
print("gui:        done")
exit(0)
