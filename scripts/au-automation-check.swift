// au-automation-check — while a host plays automation, the parameter values it reads back from an
// Audio Unit (its own knobs: Logic's Smart Controls, its generic Controls view) must follow that
// automation. An AUv3 host schedules automation into the render (AURenderEventParameter); it does
// not set the parameter tree, so a unit that answers the tree's value from anything else shows
// values the automation has already left.
//
//   swiftc -O scripts/au-automation-check.swift -o au-automation-check
//   ./au-automation-check aumu ApM3 MiSm                 AUv3, out of process (as Logic loads it)
//   ./au-automation-check aumu ApMo MiSm --in-process    AUv2 in this process
//
// For the first four parameters: set a value through the tree (a knob moved by hand), then
// schedule a ramp of other values into the render as a host's automation does, render, let the
// main thread run, and read the tree. Each must read the last automated value.
//
// Exit status 0: the tree follows the automation; 1: it does not; 2: usage or host error.

import AppKit
import AudioToolbox
import AVFoundation

func fourCC(_ s: String) -> OSType { s.utf8.reduce(0) { ($0 << 8) | OSType($1) } }

let args = CommandLine.arguments
guard args.count >= 4 else {
    print("usage: au-automation-check <type> <subtype> <manufacturer> [--in-process]")
    exit(2)
}
let inProcess = args.contains("--in-process")
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

let render = au.renderBlock
let schedule = au.scheduleParameterBlock
let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: block)!
var sampleTime = 0.0
let pull: AURenderPullInputBlock = { _, _, _, _, list in
    for b in UnsafeMutableAudioBufferListPointer(list) { if let d = b.mData { memset(d, 0, Int(b.mDataByteSize)) } }
    return noErr
}
/// Render `blocks` blocks; `before` runs ahead of each (to schedule that block's automation).
func play(_ blocks: Int, before: (Int) -> Void = { _ in }) {
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
    }
}

guard let params = au.parameterTree?.allParameters, params.count >= 4 else { print("fewer than four parameters"); exit(2) }
let tested = Array(params.prefix(4))
play(10)
pump(0.2)

var failed = false
for (k, p) in tested.enumerated() {
    let span = p.maxValue - p.minValue
    // A knob moved by hand: the value the tree knows.
    let byHand = p.minValue + span * 0.3
    p.setValue(byHand, originator: nil)
    pump(0.2)
    play(4)
    pump(0.2)
    // Automation: a ramp to 0.8 of the range, one value per block, as Logic plays a curve.
    let steps = 20
    let target = p.minValue + span * (0.8 - 0.05 * Float(k))
    play(steps) { i in
        let v = byHand + (target - byHand) * Float(i + 1) / Float(steps)
        schedule(AUEventSampleTimeImmediate, 0, p.address, v)
    }
    pump(0.3)
    play(4)
    pump(0.3)
    let read = p.value
    let ok = abs(read - target) <= span * 0.001
    print(String(format: "%@ %@: set by hand %.2f, automated to %.2f, the tree reads %.2f", ok ? "ok  " : "FAIL", p.displayName, byHand, target, read))
    if !ok { failed = true }
}
au.deallocateRenderResources()
print(failed ? "FAIL: the parameter tree does not follow the host's automation" : "OK: the parameter tree follows the host's automation")
exit(failed ? 1 : 0)
