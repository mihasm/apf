// au-state-check — a session restored into an Audio Unit must sound the same whether or not its
// editor was ever opened. Hosts restore a project's plugins with their windows closed; a sound
// that changes when the window opens is a state the plugin only half applied.
//
//   swiftc -O scripts/au-state-check.swift -o au-state-check
//   ./au-state-check aumu ApM3 MiSm                 AUv3, out of process (as Logic loads it)
//   ./au-state-check aumu ApMo MiSm --in-process    AUv2 in this process
//   options: --preset <name>   factory preset the saved session starts from (default: the second)
//            --state <plist>   restore this saved fullState instead (e.g. one taken from a project);
//                              the reference then plays it from the CLAP state alone
//            --bundle <path>   an AUv2 .component not installed: registered in this process under
//                              the given type/subtype/manufacturer (use a subtype of its own)
//            --wav <dir>       also write the last scenario's renders as 32-bit float WAVs
//
// It loads the preset into instance A, moves every parameter away from where the preset put it,
// plays, and takes A's fullState. Then two fresh instances B and C each render a little, get
// that state (as a project load does), and play the same notes; C has its editor opened (and
// closed) between the load and the notes, B never has. B and C must be bit-identical, and both
// must sound like A. No audio device is opened: the units are rendered directly.
//
// Exit status 0: identical; 1: they differ (the numbers say how); 2: usage or host error.

import AppKit
import AudioToolbox
import AVFoundation
import CoreAudioKit

func fourCC(_ s: String) -> OSType { s.utf8.reduce(0) { ($0 << 8) | OSType($1) } }

let args = CommandLine.arguments
guard args.count >= 4 else {
    print("usage: au-state-check <type> <subtype> <manufacturer> [--in-process] [--preset NAME] [--wav DIR]")
    exit(2)
}
func option(_ name: String) -> String? {
    guard let i = args.firstIndex(of: name), i + 1 < args.count else { return nil }
    return args[i + 1]
}
let inProcess = args.contains("--in-process")
let presetName = option("--preset")
let wavDir = option("--wav")
let debug = args.contains("--debug")
var desc = AudioComponentDescription(componentType: fourCC(args[1]), componentSubType: fourCC(args[2]),
                                     componentManufacturer: fourCC(args[3]), componentFlags: 0, componentFlagsMask: 0)
if let path = option("--bundle") {
    // Register the bundle's AUv2 factory in this process, so a build can be tested before it
    // is installed (and without touching the installed one a running host uses).
    guard let bundle = CFBundleCreate(nil, URL(fileURLWithPath: path) as CFURL),
          CFBundleLoadExecutable(bundle),
          let info = CFBundleGetInfoDictionary(bundle) as? [String: Any],
          let comps = info["AudioComponents"] as? [[String: Any]],
          let factoryName = comps.first?["factoryFunction"] as? String,
          let symbol = CFBundleGetFunctionPointerForName(bundle, factoryName as CFString) else {
        print("cannot load the AUv2 factory of \(path)"); exit(2)
    }
    let factory = unsafeBitCast(symbol, to: AudioComponentFactoryFunction.self)
    guard AudioComponentRegister(&desc, "au-state-check: test build" as CFString, 1, factory) != nil else {
        print("cannot register \(path)"); exit(2)
    }
}
let app = NSApplication.shared
app.setActivationPolicy(.accessory)

let sampleRate = 48_000.0
let block: AUAudioFrameCount = 512

func pump(_ seconds: Double) { RunLoop.main.run(until: Date(timeIntervalSinceNow: seconds)) }

/// The AVAudioUnits own their AudioUnits: kept alive for the whole run.
var keep: [AVAudioUnit] = []

func instantiate() -> AUAudioUnit {
    var unit: AVAudioUnit?
    var failure: Error?
    AVAudioUnit.instantiate(with: desc, options: inProcess ? [] : [.loadOutOfProcess]) { u, e in unit = u; failure = e }
    let deadline = Date(timeIntervalSinceNow: 20)
    while unit == nil && failure == nil && Date() < deadline { pump(0.02) }
    guard let unit, case let au = unit.auAudioUnit else { print("instantiate failed: \(String(describing: failure))"); exit(2) }
    keep.append(unit)
    let format = AVAudioFormat(standardFormatWithSampleRate: sampleRate, channels: 2)!
    do {
        try au.outputBusses[0].setFormat(format)
        au.maximumFramesToRender = block
        try au.allocateRenderResources()
    } catch { print("allocateRenderResources failed: \(error)"); exit(2) }
    return au
}

/// A unit and its render clock.
final class Player {
    let au: AUAudioUnit
    let render: AURenderBlock
    let buffer: AVAudioPCMBuffer
    var sampleTime = 0.0
    /// An instrument has no input, but the AUv2 bridge calls the pull block anyway.
    let pull: AURenderPullInputBlock = { _, _, _, _, list in
        for b in UnsafeMutableAudioBufferListPointer(list) { if let d = b.mData { memset(d, 0, Int(b.mDataByteSize)) } }
        return noErr
    }
    init(_ au: AUAudioUnit) {
        self.au = au
        render = au.renderBlock
        buffer = AVAudioPCMBuffer(pcmFormat: AVAudioFormat(standardFormatWithSampleRate: sampleRate, channels: 2)!, frameCapacity: block)!
    }
    /// Render `seconds` (whole blocks); `midi` is sent before the first block.
    func play(_ seconds: Double, midi: [[UInt8]] = []) -> ([Float], [Float]) {
        if let send = au.scheduleMIDIEventBlock {
            for m in midi { m.withUnsafeBufferPointer { send(AUEventSampleTimeImmediate, 0, m.count, $0.baseAddress!) } }
        }
        var l = [Float](), r = [Float]()
        let blocks = Int((seconds * sampleRate / Double(block)).rounded(.up))
        for _ in 0..<blocks {
            buffer.frameLength = block
            let abl = buffer.mutableAudioBufferList
            var flags = AudioUnitRenderActionFlags()
            var ts = AudioTimeStamp()
            ts.mSampleTime = sampleTime
            ts.mFlags = .sampleTimeValid
            let status = render(&flags, &ts, block, 0, abl, pull)
            if status != noErr { print("render failed: \(status)"); exit(2) }
            sampleTime += Double(block)
            let ch = buffer.floatChannelData!
            l.append(contentsOf: UnsafeBufferPointer(start: ch[0], count: Int(block)))
            r.append(contentsOf: UnsafeBufferPointer(start: ch[1], count: Int(block)))
        }
        return (l, r)
    }
}

/// Open the unit's editor in a window for `seconds`, then close it.
func openEditor(_ au: AUAudioUnit, seconds: Double) {
    var vc: NSViewController?
    var done = false
    au.requestViewController { c in vc = c; done = true }
    let deadline = Date(timeIntervalSinceNow: 20)
    while !done && Date() < deadline { pump(0.02) }
    // A plugin without an editor has nothing to open: wait the same time instead.
    guard let view = vc?.view else { pump(seconds); return }
    let size = view.frame.size.width > 10 ? view.frame.size : NSSize(width: 1280, height: 800)
    let window = NSWindow(contentRect: NSRect(origin: .zero, size: size), styleMask: [.titled, .closable], backing: .buffered, defer: false)
    window.contentView = view
    window.orderFront(nil)
    pump(seconds)
    window.orderOut(nil)
    window.contentView = nil
    pump(0.3)
}

// --- the saved session -------------------------------------------------------------------------

let a = Player(instantiate())
let presets = a.au.factoryPresets ?? []
let preset = presetName.flatMap { n in presets.first { $0.name == n } } ?? (presets.count > 1 ? presets[1] : presets.first)
var state: [String: Any]
if let file = option("--state") {
    // A fullState a host saved (e.g. taken from a project). The reference plays it from the
    // plugin's own data alone: what the session holds, whatever the host's copy of the
    // parameter values says.
    guard let d = FileManager.default.contents(atPath: file),
          let s = try? PropertyListSerialization.propertyList(from: d, format: nil) as? [String: Any] else { print("cannot read \(file)"); exit(2) }
    state = s
    var own = s
    if own["clapState"] != nil { own.removeValue(forKey: "data") }
    _ = a.play(0.2)
    a.au.fullState = own
    pump(0.3)
    _ = a.play(0.2)
    print("state from \(file)")
} else {
    if let preset {
        a.au.currentPreset = preset
        print("preset: \(preset.name)")
    }
    _ = a.play(0.2)
    // Every parameter somewhere the preset did not put it (a player's tweaks and automation).
    for (i, p) in (a.au.parameterTree?.allParameters ?? []).enumerated() {
        let span = p.maxValue - p.minValue
        var v = p.value + span * (i % 2 == 0 ? 0.17 : -0.13)
        if v > p.maxValue || v < p.minValue { v = p.value - span * (i % 2 == 0 ? 0.17 : -0.13) }
        p.setValue(max(p.minValue, min(p.maxValue, v)), originator: nil)
    }
    pump(0.2)
    _ = a.play(0.2)
    pump(0.2)
    if debug { print("  saved:          " + (a.au.parameterTree?.allParameters.prefix(8).map { String(format: "%.1f", $0.value) }.joined(separator: " ") ?? "")) }
    guard let s = a.au.fullState else { print("no fullState"); exit(2) }
    state = s
}
print("state: \(state.keys.sorted().joined(separator: ", "))")

let notes: [[UInt8]] = [[0x90, 45, 110], [0x90, 57, 90]]
let reference = a.play(1.5, midi: notes)
_ = a.play(0.5, midi: [[0x80, 45, 0], [0x80, 57, 0]])

func rms(_ x: [Float], _ from: Int = 0) -> Double {
    let s = x[from...]
    return (s.reduce(0.0) { $0 + Double($1 * $1) } / Double(max(1, s.count))).squareRoot()
}
func maxDiff(_ x: [Float], _ y: [Float]) -> Float { zip(x, y).reduce(0) { max($0, abs($1.0 - $1.1)) } }
/// A spectral fingerprint: RMS (dB) per octave band, 8 kHz down, from a one-pole band split,
/// over the last two thirds. Enough to tell two patches apart when oscillator phases differ.
func bands(_ x: [Float]) -> [Double] {
    var out = [Double]()
    var signal = x
    for k in 0..<8 {
        let fc = 8000.0 / pow(2.0, Double(k))
        let a = Float(exp(-2.0 * Double.pi * fc / sampleRate))
        var low = [Float](repeating: 0, count: signal.count)
        var y: Float = 0
        for i in 0..<signal.count { y = (1 - a) * signal[i] + a * y; low[i] = y }
        out.append(20 * log10(max(1e-9, rms(zip(signal, low).map { $0 - $1 }, signal.count / 3))))
        signal = low
    }
    return out
}

// --- B: restored, editor never opened; C: restored, editor opened before playing -------------
// Each scenario restores the session its own way (as Logic does at different moments); the
// editor-closed and editor-opened instances of a scenario must render the same samples.

let off: [[UInt8]] = [[0x80, 45, 0], [0x80, 57, 0]]

func restored(editor: Bool, _ restore: (Player) -> Void) -> ([Float], [Float]) {
    let p = Player(instantiate())
    restore(p)
    pump(0.3)
    _ = p.play(0.25)
    if editor { openEditor(p.au, seconds: 2.0) } else { pump(2.3) }
    _ = p.play(0.25)
    let out = p.play(1.5, midi: notes)
    _ = p.play(0.3, midi: off)
    p.au.deallocateRenderResources()
    return out
}

let otherPreset = presets.first { $0.name != preset?.name && $0.name != "Init" && $0.name != "Empty Case" }
let scenarios: [(String, (Player) -> Void)] = [
    ("project load: state after a short render", { p in _ = p.play(0.25); p.au.fullState = state }),
    ("project load: state right after allocation", { p in p.au.fullState = state }),
    ("state while render resources are deallocated", { p in
        p.au.deallocateRenderResources(); p.au.fullState = state
        if debug { print("  after state:   " + (p.au.parameterTree?.allParameters.prefix(8).map { String(format: "%.1f", $0.value) }.joined(separator: " ") ?? "")) }
        try? p.au.allocateRenderResources()
        if debug { print("  after allocate: " + (p.au.parameterTree?.allParameters.prefix(8).map { String(format: "%.1f", $0.value) }.joined(separator: " ") ?? "")) } }),
    ("state into an instance playing another sound", { p in
        if let o = otherPreset { p.au.currentPreset = o }
        _ = p.play(0.3, midi: [[0x90, 60, 100]]); _ = p.play(0.3, midi: [[0x80, 60, 0]])
        p.au.fullState = state }),
    ("state twice (copy, then undo/redo)", { p in
        _ = p.play(0.1); p.au.fullState = state; _ = p.play(0.05); p.au.fullState = state }),
    ("state, then the host re-sends every parameter", { p in
        _ = p.play(0.1); p.au.fullState = state; _ = p.play(0.05)
        for q in p.au.parameterTree?.allParameters ?? [] { q.setValue(q.value, originator: nil) } }),
    ("factory preset from the host's menu", { p in _ = p.play(0.1); if let pr = preset { p.au.currentPreset = pr } }),
]

var failures = 0
var last: (([Float], [Float]), ([Float], [Float]))? = nil
for (name, restore) in scenarios {
    let b = restored(editor: false, restore)
    let c = restored(editor: true, restore)
    let d = max(maxDiff(b.0, c.0), maxDiff(b.1, c.1))
    let fb = bands(b.0), fc = bands(c.0)
    let ref = name.hasPrefix("factory") ? fb : bands(reference.0)
    let spectral = zip(ref, fb).map { abs($0 - $1) }.max() ?? 0
    // Bit-identical, or — for a sound with random sources, which differ per instance — the
    // same spectrum closed and opened.
    let alike = d <= 1e-5 || (zip(fb, fc).map { abs($0 - $1) }.max() ?? 0) < 0.2
    let ok = alike && spectral < 1.5 && rms(b.0, Int(0.1 * sampleRate)) > 1e-3
    if !ok { failures += 1 }
    print(String(format: "%@  %@: largest sample difference closed/opened %.3g, band difference to the saved sound %.2f dB",
                 ok ? "ok  " : "FAIL", name, d, spectral))
    if !ok {
        print("      closed " + fb.map { String(format: "%6.1f", $0) }.joined(separator: " "))
        print("      opened " + fc.map { String(format: "%6.1f", $0) }.joined(separator: " "))
        print("      saved  " + ref.map { String(format: "%6.1f", $0) }.joined(separator: " "))
    }
    last = (b, c)
}

if let dir = wavDir, let (b, c) = last {
    func write(_ name: String, _ x: ([Float], [Float])) {
        let url = URL(fileURLWithPath: dir).appendingPathComponent(name)
        let f = AVAudioFormat(standardFormatWithSampleRate: sampleRate, channels: 2)!
        let buf = AVAudioPCMBuffer(pcmFormat: f, frameCapacity: AVAudioFrameCount(x.0.count))!
        buf.frameLength = AVAudioFrameCount(x.0.count)
        for i in 0..<x.0.count { buf.floatChannelData![0][i] = x.0[i]; buf.floatChannelData![1][i] = x.1[i] }
        try? AVAudioFile(forWriting: url, settings: f.settings).write(from: buf)
    }
    write("A-reference.wav", reference)
    write("B-editor-closed.wav", b)
    write("C-editor-opened.wav", c)
}

print(failures == 0 ? "OK: every restored session sounds the same with and without the editor" : "FAIL: \(failures) scenario(s) depend on the editor or lost the saved sound")
exit(failures == 0 ? 0 : 1)
