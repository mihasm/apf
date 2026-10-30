// au-legato-check — a note struck again legato (its note-off and the next note-on of the same
// key on one sample, as a DAW plays two touching notes of one pitch) must keep sounding in an
// Audio Unit instrument. clap-wrapper's AUv3 guard against reordered fast taps took such a pair
// for a reordered tap when the key had been struck in an earlier render cycle: the note-on went
// first and the note-off a sample later, so the plugin released a key the host still held (APF
// wrapper patch 0007).
//
//   swiftc -O scripts/au-legato-check.swift -o au-legato-check
//   ./au-legato-check aumu ApM3 MiSm                 AUv3, out of process (as Logic loads it)
//   ./au-legato-check aumu ApMo MiSm --in-process    AUv2 in this process
//   ./au-legato-check aumu ApM3 MiSm --preset "Bright Arp"   with a factory preset loaded
//
// A voice that pairs note-offs with note-ons by note id (CLAP's) survives the reordered pair,
// because the late off names the earlier note; a part that goes by the key alone (an
// arpeggiator's held keys) loses the key. `--preset` loads such a sound.
// Two runs from a reset: A holds C4 for 1.5 s; B holds it as well and strikes it again legato
// after 0.5 s. At the end B must be at least half as loud as A (a released note fades; a
// retriggered one starts again, as loud or louder). An instrument silent at the end of A (a
// pluck that has died away) proves nothing and passes with a note.
//
// Exit status 0: the note keeps sounding; 1: it was released; 2: usage or host error.

import AppKit
import AudioToolbox
import AVFoundation

func fourCC(_ s: String) -> OSType { s.utf8.reduce(0) { ($0 << 8) | OSType($1) } }

let args = CommandLine.arguments
guard args.count >= 4 else {
    print("usage: au-legato-check <type> <subtype> <manufacturer> [--in-process]")
    exit(2)
}
let inProcess = args.contains("--in-process")
let presetName = args.firstIndex(of: "--preset").flatMap { $0 + 1 < args.count ? args[$0 + 1] : nil }
let desc = AudioComponentDescription(componentType: fourCC(args[1]), componentSubType: fourCC(args[2]),
                                     componentManufacturer: fourCC(args[3]), componentFlags: 0, componentFlagsMask: 0)
let app = NSApplication.shared
app.setActivationPolicy(.accessory)

let sampleRate = 48_000.0
let block: AUAudioFrameCount = 512

func pump(_ seconds: Double) { RunLoop.main.run(until: Date(timeIntervalSinceNow: seconds)) }

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
if let presetName {
    guard let preset = (au.factoryPresets ?? []).first(where: { $0.name == presetName }) else {
        print("no factory preset \(presetName)"); exit(2)
    }
    au.currentPreset = preset
    pump(1.0)
}

let render = au.renderBlock
let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: block)!
var sampleTime = 0.0
let pull: AURenderPullInputBlock = { _, _, _, _, list in
    for b in UnsafeMutableAudioBufferListPointer(list) { if let d = b.mData { memset(d, 0, Int(b.mDataByteSize)) } }
    return noErr
}

/// Render `blocks` blocks; `before` runs ahead of each (to schedule its MIDI). The sum of
/// squares of the left channel over the last `tail` blocks.
@discardableResult
func play(_ blocks: Int, tail: Int = 0, before: (Int) -> Void = { _ in }) -> Double {
    var energy = 0.0
    for i in 0..<blocks {
        before(i)
        buffer.frameLength = block
        var flags = AudioUnitRenderActionFlags()
        var ts = AudioTimeStamp()
        ts.mSampleTime = sampleTime
        ts.mFlags = .sampleTimeValid
        let status = render(&flags, &ts, block, 0, buffer.mutableAudioBufferList, pull)
        if status != noErr { print("render failed: \(status)"); exit(2) }
        sampleTime += Double(block)
        if i >= blocks - tail, let l = buffer.floatChannelData?[0] {
            for n in 0..<Int(block) { energy += Double(l[n] * l[n]) }
        }
    }
    return energy
}

func send(_ bytes: [UInt8], at offset: Int) {
    var b = bytes
    midi(AUEventSampleTimeImmediate + AUEventSampleTime(offset), 0, b.count, &b)
}

let key: UInt8 = 60
let blocksPerSecond = Int(sampleRate) / Int(block)

/// One run from a reset: hold the key 1.5 s, struck again legato after 0.5 s when `legato`;
/// the RMS of the last 0.2 s.
func run(legato: Bool) -> Double {
    au.reset()
    play(blocksPerSecond)
    send([0x90, key, 100], at: 0)
    play(blocksPerSecond / 2)
    let tail = blocksPerSecond / 5
    let energy = play(blocksPerSecond, tail: tail) { i in
        if legato && i == 0 {
            // A note ending where the next of the same pitch begins: off, then on, one sample.
            send([0x80, key, 0], at: 100)
            send([0x90, key, 100], at: 100)
        }
    }
    send([0x80, key, 0], at: 0)
    play(blocksPerSecond)
    return (energy / Double(tail * Int(block))).squareRoot()
}

play(10)
pump(0.2)
let held = run(legato: false)
let again = run(legato: true)
au.deallocateRenderResources()
let db = { (x: Double) in 20 * log10(max(x, 1e-9)) }
print(String(format: "held %.1f dBFS, struck again legato %.1f dBFS (RMS of the last 0.2 s)", db(held), db(again)))
if held < 1e-4 {
    print("OK: silent while held at the end, nothing to compare")
    exit(0)
}
let ok = again >= 0.5 * held
print(ok ? "OK: a note struck again legato keeps sounding" : "FAIL: a note struck again legato was released")
exit(ok ? 0 : 1)
