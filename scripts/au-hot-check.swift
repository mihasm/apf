// au-hot-check — the hot-reload shell (`cargo xtask install <plugin> --hot`) swaps in a new
// build inside an Audio Unit while the host keeps rendering, as Logic does: the AU wrappers
// call the plugin back from their own idle timers on the main run loop, which this check runs
// between render cycles like a host's main thread.
//
//   swiftc -O scripts/au-hot-check.swift -o au-hot-check
//   ./au-hot-check aumu ApM3 MiSm --hot-dir <folder>                 AUv3, out of process
//   ./au-hot-check aumu ApMo MiSm --hot-dir <folder> --in-process    AUv2 in this process
//   options: --bundle <path>   an AUv2 .component not installed: registered in this process
//            --wait <seconds>  publish nothing; wait that long for a build published from
//                              outside (`cargo xtask hot`), to compare two different builds
//            --editor          keep the editor open in a window across the reload
//
// It holds a chord, publishes a copy of the current build under a new name (as `cargo xtask
// hot` does), and keeps rendering until the shell's log (`hot.log` in the publish folder,
// which the out-of-process AUv3 writes too) says the unit reloaded it; then plays the chord
// again. The unit must sound after the reload, and the parameter moved before it must have
// kept its value.
//
// Exit status 0: reloaded; 1: no reload or no sound after it; 2: usage or host error.

import AppKit
import AudioToolbox
import AVFoundation
import CoreAudioKit

func fourCC(_ s: String) -> OSType { s.utf8.reduce(0) { ($0 << 8) | OSType($1) } }

let args = CommandLine.arguments
guard args.count >= 4 else {
    print("usage: au-hot-check <type> <subtype> <manufacturer> --hot-dir <folder> [--in-process] [--bundle PATH] [--wait SECONDS] [--editor]")
    exit(2)
}
func option(_ name: String) -> String? {
    guard let i = args.firstIndex(of: name), i + 1 < args.count else { return nil }
    return args[i + 1]
}
guard let hotDir = option("--hot-dir") else { print("--hot-dir is required"); exit(2) }
let inProcess = args.contains("--in-process")
let waitSeconds = option("--wait").flatMap(Double.init)
let withEditor = args.contains("--editor")
var desc = AudioComponentDescription(componentType: fourCC(args[1]), componentSubType: fourCC(args[2]),
                                     componentManufacturer: fourCC(args[3]), componentFlags: 0, componentFlagsMask: 0)
if let path = option("--bundle") {
    guard let bundle = CFBundleCreate(nil, URL(fileURLWithPath: path) as CFURL),
          CFBundleLoadExecutable(bundle),
          let info = CFBundleGetInfoDictionary(bundle) as? [String: Any],
          let comps = info["AudioComponents"] as? [[String: Any]],
          let factoryName = comps.first?["factoryFunction"] as? String,
          let symbol = CFBundleGetFunctionPointerForName(bundle, factoryName as CFString) else {
        print("cannot load the AUv2 factory of \(path)"); exit(2)
    }
    let factory = unsafeBitCast(symbol, to: AudioComponentFactoryFunction.self)
    guard AudioComponentRegister(&desc, "au-hot-check: test build" as CFString, 1, factory) != nil else {
        print("cannot register \(path)"); exit(2)
    }
}
setvbuf(stdout, nil, _IOLBF, 0)
let app = NSApplication.shared
app.setActivationPolicy(.accessory)

let sampleRate = 48_000.0
let block: AUAudioFrameCount = 512
let blocksPerSecond = Int(sampleRate) / Int(block)

func pump(_ seconds: Double) { RunLoop.main.run(until: Date(timeIntervalSinceNow: seconds)) }

func shell(_ tool: String, _ arguments: [String]) -> Bool {
    let p = Process()
    p.executableURL = URL(fileURLWithPath: "/usr/bin/env")
    p.arguments = [tool] + arguments
    p.standardOutput = Pipe()
    p.standardError = Pipe()
    do { try p.run() } catch { return false }
    p.waitUntilExit()
    return p.terminationStatus == 0
}

/// Publish a copy of the current build under a new name; returns the name.
func republish() -> String? {
    let dir = URL(fileURLWithPath: hotDir)
    guard let current = try? String(contentsOf: dir.appendingPathComponent("current"), encoding: .utf8)
        .trimmingCharacters(in: .whitespacesAndNewlines), let dot = current.lastIndex(of: ".") else { return nil }
    // A copy of a copy is named after the original build.
    let stem = current[..<dot].components(separatedBy: "-au-hot-check")[0].components(separatedBy: "-hot-test")[0]
    let name = "\(stem)-au-hot-check-\(getpid())\(current[dot...])"
    let dst = dir.appendingPathComponent(name)
    try? FileManager.default.removeItem(at: dst)
    guard (try? FileManager.default.copyItem(at: dir.appendingPathComponent(current), to: dst)) != nil,
          shell("install_name_tool", ["-id", "@rpath/\(name)", dst.path]),
          shell("codesign", ["--force", "--sign", "-", dst.path]) else { return nil }
    let tmp = dir.appendingPathComponent("current.tmp")
    guard (try? name.write(to: tmp, atomically: false, encoding: .utf8)) != nil,
          rename(tmp.path, dir.appendingPathComponent("current").path) == 0 else { return nil }
    return name
}

/// The lines of the shell's log written since `offset` bytes.
func logLines(from offset: Int) -> [String] {
    guard let data = FileManager.default.contents(atPath: hotDir + "/hot.log"), data.count > offset else { return [] }
    return String(decoding: data[offset...], as: UTF8.self).split(separator: "\n").map(String.init)
}
let logStart = FileManager.default.contents(atPath: hotDir + "/hot.log")?.count ?? 0

var unit: AVAudioUnit?
var failure: Error?
AVAudioUnit.instantiate(with: desc, options: inProcess ? [] : [.loadOutOfProcess]) { u, e in unit = u; failure = e }
let deadline = Date(timeIntervalSinceNow: 20)
while unit == nil && failure == nil && Date() < deadline { pump(0.02) }
guard let unit else { print("instantiate failed: \(String(describing: failure))"); exit(2) }
let au = unit.auAudioUnit
let format = AVAudioFormat(standardFormatWithSampleRate: sampleRate, channels: 2)!
do {
    try au.outputBusses[0].setFormat(format)
    au.maximumFramesToRender = block
    try au.allocateRenderResources()
} catch { print("allocateRenderResources failed: \(error)"); exit(2) }
guard let midi = au.scheduleMIDIEventBlock else { print("the unit takes no MIDI"); exit(2) }

var window: NSWindow?
if withEditor {
    var vc: NSViewController?
    var done = false
    au.requestViewController { c in vc = c; done = true }
    let until = Date(timeIntervalSinceNow: 10)
    while !done && Date() < until { pump(0.02) }
    if let view = vc?.view {
        let size = view.frame.size.width > 10 ? view.frame.size : NSSize(width: 1280, height: 800)
        let w = NSWindow(contentRect: NSRect(origin: .zero, size: size), styleMask: [.titled], backing: .buffered, defer: false)
        w.contentView = view
        w.orderFrontRegardless()
        window = w
    } else {
        print("the unit gave no editor"); exit(2)
    }
}

// A parameter moved away from its default; the reload must keep it.
guard let probe = au.parameterTree?.allParameters.first(where: { $0.maxValue > $0.minValue }) else {
    print("the unit has no parameter"); exit(2)
}
let moved: AUValue = abs(probe.value - probe.minValue) > abs(probe.value - probe.maxValue) ? probe.minValue : probe.maxValue
probe.value = moved
pump(0.3)

let render = au.renderBlock
let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: block)!
var sampleTime = 0.0
let pull: AURenderPullInputBlock = { _, _, _, _, list in
    for b in UnsafeMutableAudioBufferListPointer(list) { if let d = b.mData { memset(d, 0, Int(b.mDataByteSize)) } }
    return noErr
}

/// One render cycle, then the main run loop for a moment (the host's two threads in turn);
/// returns the block's peak.
func cycle(_ before: () -> Void = {}) -> Float {
    before()
    buffer.frameLength = block
    var flags = AudioUnitRenderActionFlags()
    var ts = AudioTimeStamp()
    ts.mSampleTime = sampleTime
    ts.mFlags = .sampleTimeValid
    let status = render(&flags, &ts, block, 0, buffer.mutableAudioBufferList, pull)
    if status != noErr { print("render failed: \(status)"); exit(2) }
    sampleTime += Double(block)
    pump(0.002)
    var peak: Float = 0
    for ch in 0..<Int(format.channelCount) {
        if let d = buffer.floatChannelData?[ch] { for n in 0..<Int(block) { peak = max(peak, abs(d[n])) } }
    }
    return peak
}
func chord(_ status: UInt8) {
    for key: UInt8 in [60, 64, 67] {
        var b: [UInt8] = [status, key, status == 0x90 ? 100 : 0]
        midi(AUEventSampleTimeImmediate, 0, b.count, &b)
    }
}
let db = { (x: Float) in 20 * log10(max(Double(x), 1e-9)) }

var before: Float = 0
for i in 0..<blocksPerSecond { before = max(before, cycle { if i == 0 { chord(0x90) } }) }

let published: String
if let waitSeconds {
    print("waiting up to \(Int(waitSeconds)) s for a build published from outside")
    published = ""
} else {
    guard let name = republish() else { print("cannot publish a copy of the current build in \(hotDir)"); exit(2) }
    print("published \(name)")
    published = name
}
let start = Date()
let limit = waitSeconds ?? 10
var reloadLine: String?
var blocks = 0
while reloadLine == nil {
    if Date().timeIntervalSince(start) > limit {
        print("FAIL: no reload within \(Int(limit)) s; the shell's log since the start:")
        logLines(from: logStart).forEach { print("  \($0)") }
        exit(1)
    }
    _ = cycle()
    blocks += 1
    reloadLine = logLines(from: logStart).first { $0.contains("reloaded") && (published.isEmpty || $0.contains(published)) }
}
print("\(reloadLine!)")
print(String(format: "reloaded %.0f ms after publishing, %d render cycles meanwhile", Date().timeIntervalSince(start) * 1000, blocks))
pump(0.3)
let kept = probe.value
print("\(probe.displayName): \(kept) after the reload (set before: \(moved))")
var after: Float = 0
for i in 0..<blocksPerSecond {
    after = max(after, cycle {
        if i == 0 { chord(0x80) }
        if i == 1 { chord(0x90) }
    })
}
chord(0x80)
print(String(format: "peak before %.1f dBFS, after %.1f dBFS", db(before), db(after)))
window?.close()
au.deallocateRenderResources()
if abs(kept - moved) > 1e-4 * max(1, probe.maxValue - probe.minValue) {
    print("FAIL: \(probe.displayName) lost its value in the reload")
    exit(1)
}
if !(after.isFinite && after > 1e-4) {
    print("FAIL: no sound after the reload")
    exit(1)
}
print("OK: the unit swapped in the new build while rendering, kept its parameter and sounds")
exit(0)
