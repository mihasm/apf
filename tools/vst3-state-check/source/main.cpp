// vst3-state-check — a session restored into a VST3 plug-in must sound as it was saved, in
// every order a host restores it, and the values its controller shows must be the restored
// ones. The VST3 counterpart of scripts/au-state-check.swift.
//
//   vst3-state-check <Plugin.vst3> [options]
//     --preset-program N  program (factory preset) instance A starts from (default: the second)
//     --preset NAME       the same, by name
//     --state FILE        restore this state instead of A's edits: a .vstpreset or the raw
//                         component state a host saved
//     --save-state FILE   write A's state as a .vstpreset
//     --view            also run every scenario with the view opened (off screen) and
//                         closed again between the restore and the notes
//     --wav DIR           write the renders as 32-bit float WAVs
//     --inject-stale      check the check: after every restore the host sends the processor
//                         the parameters' defaults (as the AU bug of 2026-10-25 did with old
//                         values); every restore scenario must then FAIL
//
// Instance A: 48 kHz, 512-sample blocks, activated; it plays a little, gets the program, then
// every parameter is moved through the controller and sent to the processor as a parameter
// change (as a host does), it plays on, and its component and controller states are taken.
// Then it plays a few notes: the reference. Fresh instances B get that state in the orders
// hosts restore sessions in and play the same notes. For every B:
//   - the values its controller shows are the saved ones, and its component state, taken
//     again before the notes, is byte for byte the restored one (the processor holds them);
//   - its sound is A's: bit-identical (max abs difference <= 1e-5) when B rendered as many
//     samples as A and the plug-in's sound does not depend on when a sound was loaded,
//     otherwise the same spectrum (1.5 dB per octave band);
//   - where a scenario has a twin (the same calls at the same moments, without what must not
//     matter: the host's re-send, the controller's extra calls, the order around activation),
//     the two are bit-identical.
// What can be asked is measured first: a second instance given A's history (the same samples,
// unless the sound has random sources: then twins compare by spectrum, 0.2 dB), and the
// program loaded at two moments (the same samples, unless the load time matters).
// No audio device is opened; everything renders offline in this process. An effect (a main audio
// input, and the Fx category or no event input) gets a test signal on that input: a sine in every
// octave band, its phase set by the absolute sample position alone. Instruments get silence.
//
// Exit status 0: every scenario passed; 1: one failed (the numbers say how); 2: usage or host
// error.

#include "platform.h"

#include "public.sdk/source/common/memorystream.h"
#include "public.sdk/source/vst/hosting/eventlist.h"
#include "public.sdk/source/vst/hosting/hostclasses.h"
#include "public.sdk/source/vst/hosting/module.h"
#include "public.sdk/source/vst/hosting/parameterchanges.h"
#include "public.sdk/source/vst/hosting/plugprovider.h"
#include "public.sdk/source/vst/hosting/processdata.h"
#include "public.sdk/source/vst/utility/stringconvert.h"
#include "pluginterfaces/base/funknownimpl.h"
#include "pluginterfaces/gui/iplugview.h"
#include "pluginterfaces/vst/ivstaudioprocessor.h"
#include "pluginterfaces/vst/ivstcomponent.h"
#include "pluginterfaces/vst/ivsteditcontroller.h"
#include "pluginterfaces/vst/ivstevents.h"
#include "pluginterfaces/vst/ivstmidicontrollers.h"
#include "pluginterfaces/vst/ivstprocesscontext.h"
#include "pluginterfaces/vst/ivstunits.h"

#include <algorithm>
#include <cctype>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <fstream>
#include <functional>
#include <iostream>
#include <iterator>
#include <memory>
#include <set>
#include <string>
#include <vector>

using namespace Steinberg;
using namespace Steinberg::Vst;

namespace {

constexpr double kRate = 48000.0;
constexpr double kPi = 3.14159265358979323846;
constexpr int32 kBlock = 512;

// Instance A's history, in blocks since creation.
constexpr int64 kProgramAt = 4;  // the program is selected
constexpr int64 kEditAt = 24;    // every parameter moves (or --state is restored)
constexpr int64 kSaveAt = 48;    // the state is taken
constexpr int64 kNotesAt = 72;   // the notes start: the reference
constexpr int32 kNoteBlocks = 141;  // 1.5 s of notes
constexpr int32 kOffBlocks = 47;    // 0.5 s after the note-offs
constexpr int64 kSettle = 36;       // blocks a restored instance plays at least before the notes

using Audio = std::pair<std::vector<float>, std::vector<float>>;

struct Note
{
    int16 pitch;
    float velocity; // 0: note off
};
const std::vector<Note> kNotes = {{45, 110.f / 127.f}, {57, 90.f / 127.f}};
const std::vector<Note> kNotesOff = {{45, 0.f}, {57, 0.f}};

struct State
{
    std::vector<char> component, controller;
};

IPtr<MemoryStream> streamOf(const std::vector<char>& bytes)
{
    auto s = owned(new MemoryStream);
    if (!bytes.empty())
    {
        int32 written = 0;
        s->write(const_cast<char*>(bytes.data()), static_cast<int32>(bytes.size()), &written);
    }
    s->seek(0, IBStream::kIBSeekSet, nullptr);
    return s;
}

std::vector<char> bytesOf(MemoryStream& s)
{
    return std::vector<char>(s.getData(), s.getData() + s.getSize());
}

std::string str(const TChar* s) { return Steinberg::Vst::StringConvert::convert(s); }

/// The test signal an effect gets on its main input, at the absolute sample position `n`: a sine
/// in every octave band the sounds are compared in. It depends on `n` alone, so every instance,
/// scenario and render feeds the plug-in the same samples at the same moments.
float testSignal(int64 n)
{
    static const double kFrequencies[] = {90, 180, 360, 720, 1440, 2880, 5760, 11520};
    double s = 0;
    for (double f : kFrequencies) s += std::sin(2 * kPi * std::fmod(f * double(n) / kRate, 1.0));
    return float(0.05 * s);
}

// --- the host's side of the plug-in -----------------------------------------------------------

/// What a host gives the controller: edits from the plug-in (its view, a preset it loaded)
/// reach the processor as parameter changes in the next block, as in any host.
class Handler : public U::Implements<U::Directly<IComponentHandler>>
{
public:
    std::function<void(ParamID, ParamValue)> edit;
    int restarts = 0;
    tresult PLUGIN_API beginEdit(ParamID) override { return kResultOk; }
    tresult PLUGIN_API performEdit(ParamID id, ParamValue value) override
    {
        if (edit) edit(id, value);
        return kResultOk;
    }
    tresult PLUGIN_API endEdit(ParamID) override { return kResultOk; }
    tresult PLUGIN_API restartComponent(int32) override
    {
        ++restarts;
        return kResultOk;
    }
};

class Frame : public U::Implements<U::Directly<IPlugFrame>>
{
public:
    tresult PLUGIN_API resizeView(IPlugView* view, ViewRect* rect) override
    {
        return view && rect ? view->onSize(rect) : kInvalidArgument;
    }
};

/// Whether the plug-in is an effect, whose sound comes from its audio input: its first audio
/// input is a main bus, and its class is in the Fx category (and not an instrument) or it takes
/// no events.
bool isEffect(IComponent& component, const VST3::Hosting::ClassInfo& info)
{
    BusInfo bus{};
    if (component.getBusCount(kAudio, kInput) < 1 || component.getBusInfo(kAudio, kInput, 0, bus) != kResultOk ||
        bus.busType != kMain)
        return false;
    const auto& sub = info.subCategories();
    bool fx = std::find(sub.begin(), sub.end(), PlugType::kFx) != sub.end();
    bool instrument = std::find(sub.begin(), sub.end(), PlugType::kInstrument) != sub.end();
    return (fx && !instrument) || component.getBusCount(kEvent, kInput) == 0;
}

/// One plug-in instance and its render clock.
class Instance
{
public:
    Instance(const VST3::Hosting::PluginFactory& factory, const VST3::Hosting::ClassInfo& info)
    {
        provider = owned(new PlugProvider(factory, info, true));
        if (!provider->initialize()) return;
        component = provider->getComponentPtr();
        controller = provider->getControllerPtr();
        processor = U::cast<IAudioProcessor>(component);
        if (!component || !controller || !processor) return;
        single = U::cast<IEditController>(component) != nullptr;
        effect = isEffect(*component, info);
        handler = owned(new Handler);
        handler->edit = [this](ParamID id, ParamValue v) { queue(id, v); };
        controller->setComponentHandler(handler);
        // As every host does after creating a plug-in: the controller learns the component's
        // initial state.
        auto s = owned(new MemoryStream);
        if (component->getState(s) == kResultOk)
        {
            s->seek(0, IBStream::kIBSeekSet, nullptr);
            controller->setComponentState(s);
        }
        ok = true;
    }

    ~Instance()
    {
        if (component && active) deactivate();
        if (controller) controller->setComponentHandler(nullptr);
        data.unprepare();
        processor = nullptr;
        controller = nullptr;
        component = nullptr;
        provider = nullptr;
    }

    /// Buses, setupProcessing and buffers: what a host does before it activates.
    bool prepare()
    {
        for (auto dir : {kInput, kOutput})
        {
            if (component->getBusCount(kAudio, dir) > 0) component->activateBus(kAudio, dir, 0, true);
            if (component->getBusCount(kEvent, dir) > 0) component->activateBus(kEvent, dir, 0, true);
        }
        ProcessSetup setup{kRealtime, kSample32, kBlock, kRate};
        if (processor->setupProcessing(setup) != kResultOk) return false;
        if (!data.prepare(*component, kBlock, kSample32)) return false;
        int32 n = controller->getParameterCount();
        in.setMaxParameters(n + 1);
        out.setMaxParameters(n + 1);
        data.inputEvents = &events;
        data.inputParameterChanges = &in;
        data.outputParameterChanges = &out;
        data.processContext = &context;
        prepared = true;
        return true;
    }

    bool activate()
    {
        if (!prepared && !prepare()) return false;
        if (component->setActive(true) != kResultOk) return false;
        processor->setProcessing(true); // a host goes on whatever it answers
        active = true;
        return true;
    }

    void deactivate()
    {
        processor->setProcessing(false);
        component->setActive(false);
        active = false;
    }

    /// A parameter change for the processor in the next block.
    void queue(ParamID id, ParamValue value)
    {
        for (auto& p : pending)
            if (p.first == id)
            {
                p.second = value;
                return;
            }
        pending.emplace_back(id, value);
    }

    /// The host sets a parameter: on the controller, and to the processor in the next block.
    void set(ParamID id, ParamValue value)
    {
        controller->setParamNormalized(id, value);
        queue(id, value);
    }

    /// One block; `notes` start at its first sample.
    void block(const std::vector<Note>& notes = {}, Audio* into = nullptr)
    {
        events.clear();
        in.clearQueue();
        out.clearQueue();
        for (auto& p : pending)
        {
            int32 index = 0;
            if (auto* q = in.addParameterData(p.first, index)) q->addPoint(0, p.second, index);
        }
        pending.clear();
        for (auto& n : notes)
        {
            Event e{};
            e.busIndex = 0;
            e.sampleOffset = 0;
            if (n.velocity > 0)
            {
                e.type = Event::kNoteOnEvent;
                e.noteOn.channel = 0;
                e.noteOn.pitch = n.pitch;
                e.noteOn.velocity = n.velocity;
                e.noteOn.noteId = -1;
            }
            else
            {
                e.type = Event::kNoteOffEvent;
                e.noteOff.channel = 0;
                e.noteOff.pitch = n.pitch;
                e.noteOff.noteId = -1;
            }
            events.addEvent(e);
        }
        // An effect's main input gets the test signal; every other input gets silence.
        for (int32 b = 0; b < data.numInputs; ++b)
            for (int32 c = 0; c < data.inputs[b].numChannels; ++c)
            {
                float* x = data.inputs[b].channelBuffers32[c];
                if (effect && b == 0)
                    for (int32 i = 0; i < kBlock; ++i) x[i] = testSignal(rendered + i);
                else
                    std::memset(x, 0, sizeof(float) * kBlock);
            }
        context = {};
        context.state = ProcessContext::kTempoValid | ProcessContext::kTimeSigValid |
                        ProcessContext::kContTimeValid | ProcessContext::kProjectTimeMusicValid;
        context.sampleRate = kRate;
        context.projectTimeSamples = rendered;
        context.continousTimeSamples = rendered;
        context.tempo = 120;
        context.projectTimeMusic = rendered / kRate * 2.0;
        context.timeSigNumerator = 4;
        context.timeSigDenominator = 4;
        data.numSamples = kBlock;
        processor->process(data);
        rendered += kBlock;
        // Values the processor reports go to the controller, as hosts transfer them.
        for (int32 i = 0; i < out.getParameterCount(); ++i)
        {
            auto* q = out.getParameterData(i);
            int32 offset = 0;
            ParamValue v = 0;
            if (q && q->getPointCount() > 0 && q->getPoint(q->getPointCount() - 1, offset, v) == kResultOk)
                controller->setParamNormalized(q->getParameterId(), v);
        }
        if (into && data.numOutputs > 0)
        {
            auto& bus = data.outputs[0];
            float* l = bus.numChannels > 0 ? bus.channelBuffers32[0] : nullptr;
            float* r = bus.numChannels > 1 ? bus.channelBuffers32[1] : l;
            if (l)
            {
                into->first.insert(into->first.end(), l, l + kBlock);
                into->second.insert(into->second.end(), r, r + kBlock);
            }
        }
        // The host's main thread runs between audio blocks.
        platform::pump(0);
    }

    Audio render(int32 blocks, const std::vector<Note>& notes = {})
    {
        Audio a;
        for (int32 i = 0; i < blocks; ++i) block(i == 0 ? notes : std::vector<Note>{}, &a);
        return a;
    }

    void renderUntil(int64 blocks)
    {
        while (rendered < blocks * kBlock) block();
    }

    State save()
    {
        State st;
        auto c = owned(new MemoryStream);
        if (component->getState(c) == kResultOk) st.component = bytesOf(*c);
        auto e = owned(new MemoryStream);
        if (controller->getState(e) == kResultOk) st.controller = bytesOf(*e);
        return st;
    }

    /// IComponent::setState alone. A plug-in with a separate controller cannot know the state
    /// otherwise, so its controller gets setComponentState too.
    void setState(const State& st)
    {
        component->setState(streamOf(st.component));
        if (!single) controller->setComponentState(streamOf(st.component));
    }

    /// The whole restore of a project load: the component's state, the controller's
    /// setComponentState with the same bytes, then the controller's own state.
    void restoreAsHost(const State& st)
    {
        component->setState(streamOf(st.component));
        controller->setComponentState(streamOf(st.component));
        if (!st.controller.empty()) controller->setState(streamOf(st.controller));
    }

    bool ok = false;
    bool single = false;
    bool effect = false; // the main input gets the test signal
    bool active = false;
    bool prepared = false;
    int64 rendered = 0;
    IPtr<PlugProvider> provider;
    IPtr<IComponent> component;
    IPtr<IEditController> controller;
    IPtr<IAudioProcessor> processor;
    IPtr<Handler> handler;
    HostProcessData data;
    EventList events{128};
    ParameterChanges in, out;
    ProcessContext context{};
    std::vector<std::pair<ParamID, ParamValue>> pending;
};

// --- what the plug-in shows ---------------------------------------------------------------------

struct Param
{
    ParamID id;
    std::string name;
    int32 steps;
    int32 flags;
    UnitID unit;
    bool midi; // a MIDI controller the host maps (IMidiMapping): not part of the state
    double defaultValue;
};

struct Catalog
{
    std::vector<Param> params;
    ParamID programParam = kNoParamId;
    int32 programSteps = 0;
    std::vector<std::string> programs;

    /// The parameters a player and automation move: not MIDI controllers, programs,
    /// read-only values or the bypass.
    bool editable(const Param& p) const
    {
        return !p.midi && !(p.flags & (ParameterInfo::kIsProgramChange | ParameterInfo::kIsReadOnly |
                                       ParameterInfo::kIsBypass));
    }
    /// The values a session holds, compared after a restore (the bypass too).
    bool stateful(const Param& p) const
    {
        return !p.midi && !(p.flags & (ParameterInfo::kIsProgramChange | ParameterInfo::kIsReadOnly));
    }
    /// What a host shows and re-sends: everything but the MIDI controllers and read-only values.
    bool shown(const Param& p) const
    {
        return !p.midi && !(p.flags & ParameterInfo::kIsReadOnly);
    }
};

Catalog catalog(Instance& inst)
{
    Catalog c;
    std::set<ParamID> midi;
    if (auto mapping = U::cast<IMidiMapping>(inst.controller))
    {
        for (int16 ch = 0; ch < 16; ++ch)
            for (CtrlNumber cc = 0; cc < kCountCtrlNumber; ++cc)
            {
                ParamID id = kNoParamId;
                if (mapping->getMidiControllerAssignment(0, ch, cc, id) == kResultTrue) midi.insert(id);
            }
    }
    for (int32 i = 0; i < inst.controller->getParameterCount(); ++i)
    {
        ParameterInfo info{};
        if (inst.controller->getParameterInfo(i, info) != kResultOk) continue;
        c.params.push_back({info.id, str(info.title), info.stepCount, info.flags, info.unitId, midi.count(info.id) > 0,
                            info.defaultNormalizedValue});
    }
    // The plug-in's programs, as hosts find them: the root unit's program list, selected by
    // the root unit's program change parameter.
    auto units = U::cast<IUnitInfo>(inst.controller);
    if (!units) return c;
    ProgramListID listId = kNoProgramListId;
    for (int32 i = 0; i < units->getUnitCount(); ++i)
    {
        UnitInfo unit{};
        if (units->getUnitInfo(i, unit) == kResultOk && unit.id == kRootUnitId) listId = unit.programListId;
    }
    if (listId == kNoProgramListId) return c;
    for (int32 i = 0; i < units->getProgramListCount(); ++i)
    {
        ProgramListInfo list{};
        if (units->getProgramListInfo(i, list) != kResultOk || list.id != listId) continue;
        for (int32 k = 0; k < list.programCount; ++k)
        {
            String128 name{};
            units->getProgramName(list.id, k, name);
            c.programs.push_back(str(name));
        }
    }
    for (auto& p : c.params)
        if (!p.midi && (p.flags & ParameterInfo::kIsProgramChange) && p.unit == kRootUnitId)
        {
            c.programParam = p.id;
            c.programSteps = p.steps;
            break;
        }
    return c;
}

std::vector<double> values(Instance& inst, const Catalog& c)
{
    std::vector<double> v;
    for (auto& p : c.params) v.push_back(inst.controller->getParamNormalized(p.id));
    return v;
}

/// The host selects a program from the plug-in's list: through the controller and as a
/// parameter change, then gives the main thread time (a wrapper loads presets there).
void selectProgram(Instance& inst, const Catalog& c, int32 index)
{
    if (c.programParam == kNoParamId) return;
    ParamValue v = c.programSteps > 0 ? double(index) / c.programSteps : 0.0;
    inst.set(c.programParam, v);
    inst.block();
    platform::pump(0.3);
}

/// Every editable parameter somewhere the program did not put it (a player's tweaks and
/// automation): +0.17 or -0.13 of the range, the other way where that leaves it.
std::vector<std::pair<ParamID, ParamValue>> edits(Instance& inst, const Catalog& c)
{
    std::vector<std::pair<ParamID, ParamValue>> e;
    int i = 0;
    for (auto& p : c.params)
    {
        if (!c.editable(p)) continue;
        double cur = inst.controller->getParamNormalized(p.id);
        double d = (i++ % 2 == 0) ? 0.17 : -0.13;
        double v = cur + d;
        if (v > 1.0 || v < 0.0) v = cur - d;
        v = std::clamp(v, 0.0, 1.0);
        if (p.steps > 0)
        {
            v = std::round(v * p.steps) / p.steps;
            if (std::abs(v - cur) < 0.5 / p.steps) v = cur + (cur < 0.5 ? 1.0 : -1.0) / p.steps;
            v = std::clamp(v, 0.0, 1.0);
        }
        e.emplace_back(p.id, v);
    }
    return e;
}

// --- comparing sounds -------------------------------------------------------------------------

double rms(const std::vector<float>& x, size_t from = 0)
{
    double s = 0;
    for (size_t i = from; i < x.size(); ++i) s += double(x[i]) * x[i];
    return x.size() > from ? std::sqrt(s / double(x.size() - from)) : 0.0;
}

float maxDiff(const Audio& a, const Audio& b)
{
    float d = 0;
    size_t n = std::min(a.first.size(), b.first.size());
    if (a.first.size() != b.first.size()) return INFINITY;
    for (size_t i = 0; i < n; ++i)
        d = std::max({d, std::abs(a.first[i] - b.first[i]), std::abs(a.second[i] - b.second[i])});
    return d;
}

/// A spectral fingerprint: RMS (dB) per octave band, 8 kHz down, from a one-pole band split,
/// over the last two thirds (au-state-check's, unchanged).
std::vector<double> bands(const std::vector<float>& x)
{
    std::vector<double> out;
    std::vector<float> signal = x;
    for (int k = 0; k < 8; ++k)
    {
        double fc = 8000.0 / std::pow(2.0, k);
        float a = float(std::exp(-2.0 * kPi * fc / kRate));
        std::vector<float> low(signal.size()), high(signal.size());
        float y = 0;
        for (size_t i = 0; i < signal.size(); ++i)
        {
            y = (1 - a) * signal[i] + a * y;
            low[i] = y;
            high[i] = signal[i] - y;
        }
        out.push_back(20 * std::log10(std::max(1e-9, rms(high, signal.size() / 3))));
        signal = low;
    }
    return out;
}

double bandDiff(const std::vector<double>& a, const std::vector<double>& b)
{
    double d = 0;
    for (size_t i = 0; i < a.size() && i < b.size(); ++i) d = std::max(d, std::abs(a[i] - b[i]));
    return d;
}

std::string bandLine(const std::vector<double>& b)
{
    std::string s;
    char buf[16];
    for (double v : b)
    {
        std::snprintf(buf, sizeof buf, "%7.1f", v);
        s += buf;
    }
    return s;
}

void writeWav(const std::string& path, const Audio& a)
{
    std::ofstream f(path, std::ios::binary);
    auto u32 = [&](uint32_t v) { f.write(reinterpret_cast<const char*>(&v), 4); };
    auto u16 = [&](uint16_t v) { f.write(reinterpret_cast<const char*>(&v), 2); };
    uint32_t frames = uint32_t(a.first.size());
    uint32_t bytes = frames * 2 * 4;
    f.write("RIFF", 4);
    u32(36 + bytes);
    f.write("WAVEfmt ", 8);
    u32(16);
    u16(3); // IEEE float
    u16(2);
    u32(uint32_t(kRate));
    u32(uint32_t(kRate) * 8);
    u16(8);
    u16(32);
    f.write("data", 4);
    u32(bytes);
    for (uint32_t i = 0; i < frames; ++i)
    {
        f.write(reinterpret_cast<const char*>(&a.first[i]), 4);
        f.write(reinterpret_cast<const char*>(&a.second[i]), 4);
    }
}

// --- .vstpreset (Steinberg's preset file: header, chunks, chunk list) -------------------------

template <typename T> T readLE(const std::vector<char>& b, size_t at)
{
    T v{};
    std::memcpy(&v, b.data() + at, sizeof(T)); // the format is little-endian, like every host CPU
    return v;
}

bool readState(const std::string& path, State& st, std::string& error)
{
    std::ifstream f(path, std::ios::binary);
    if (!f)
    {
        error = "cannot read " + path;
        return false;
    }
    std::vector<char> b((std::istreambuf_iterator<char>(f)), std::istreambuf_iterator<char>());
    if (b.size() >= 48 && std::memcmp(b.data(), "VST3", 4) == 0)
    {
        auto list = readLE<int64_t>(b, 40);
        if (list < 0 || size_t(list) + 8 > b.size() || std::memcmp(b.data() + list, "List", 4) != 0)
        {
            error = path + ": no chunk list";
            return false;
        }
        auto count = readLE<int32_t>(b, size_t(list) + 4);
        for (int32_t i = 0; i < count; ++i)
        {
            size_t e = size_t(list) + 8 + size_t(i) * 20;
            if (e + 20 > b.size()) break;
            auto offset = readLE<int64_t>(b, e + 4);
            auto size = readLE<int64_t>(b, e + 12);
            if (offset < 0 || size < 0 || size_t(offset + size) > b.size()) continue;
            std::vector<char> chunk(b.begin() + offset, b.begin() + offset + size);
            if (std::memcmp(b.data() + e, "Comp", 4) == 0) st.component = chunk;
            if (std::memcmp(b.data() + e, "Cont", 4) == 0) st.controller = chunk;
        }
        if (st.component.empty())
        {
            error = path + ": no component state";
            return false;
        }
        return true;
    }
    st.component = b;
    return true;
}

bool writeState(const std::string& path, const State& st, const VST3::UID& uid)
{
    std::vector<char> b;
    auto put = [&](const void* p, size_t n) { b.insert(b.end(), (const char*)p, (const char*)p + n); };
    auto i32 = [&](int32_t v) { put(&v, 4); };
    auto i64 = [&](int64_t v) { put(&v, 8); };
    char cid[33] = {};
    FUID::fromTUID(uid.data()).toString(cid);
    put("VST3", 4);
    i32(1);
    put(cid, 32);
    i64(0); // the chunk list's offset, filled in below
    struct Entry
    {
        const char* id;
        int64_t offset, size;
    };
    std::vector<Entry> entries;
    entries.push_back({"Comp", int64_t(b.size()), int64_t(st.component.size())});
    put(st.component.data(), st.component.size());
    if (!st.controller.empty())
    {
        entries.push_back({"Cont", int64_t(b.size()), int64_t(st.controller.size())});
        put(st.controller.data(), st.controller.size());
    }
    int64_t list = int64_t(b.size());
    std::memcpy(b.data() + 40, &list, 8);
    put("List", 4);
    i32(int32_t(entries.size()));
    for (auto& e : entries)
    {
        put(e.id, 4);
        i64(e.offset);
        i64(e.size);
    }
    std::ofstream f(path, std::ios::binary);
    f.write(b.data(), std::streamsize(b.size()));
    return bool(f);
}

std::string slug(const std::string& s)
{
    std::string o;
    for (char ch : s)
        o += std::isalnum(static_cast<unsigned char>(ch)) ? ch : '-';
    return o;
}

} // namespace

int main(int argc, char* argv[])
{
    std::vector<std::string> args(argv + 1, argv + argc);
    auto option = [&](const std::string& name) -> std::string {
        auto it = std::find(args.begin(), args.end(), name);
        return it != args.end() && it + 1 != args.end() ? *(it + 1) : std::string();
    };
    auto flag = [&](const std::string& name) { return std::find(args.begin(), args.end(), name) != args.end(); };
    if (args.empty() || args[0].rfind("--", 0) == 0)
    {
        std::printf("usage: vst3-state-check <Plugin.vst3> [--preset-program N | --preset NAME] [--state FILE]\n"
                    "                        [--save-state FILE] [--view] [--wav DIR] [--inject-stale]\n");
        return 2;
    }
    const std::string path = args[0];
    const std::string presetName = option("--preset");
    const std::string programText = option("--preset-program");
    const std::string stateFile = option("--state");
    const std::string saveFile = option("--save-state");
    const std::string wavDir = option("--wav");
    const bool view = flag("--view");
    const bool injectStale = flag("--inject-stale");

    platform::init();
    auto hostApp = owned(new HostApplication);
    PluginContextFactory::instance().setPluginContext(hostApp);
    PlugProvider::setErrorStream(&std::cerr);

    std::string error;
    auto module = VST3::Hosting::Module::create(path, error);
    if (!module)
    {
        std::printf("cannot load %s: %s\n", path.c_str(), error.c_str());
        return 2;
    }
    auto factory = module->getFactory();
    VST3::Hosting::ClassInfo classInfo;
    bool found = false;
    for (auto& ci : factory.classInfos())
        if (ci.category() == kVstAudioEffectClass)
        {
            classInfo = ci;
            found = true;
            break;
        }
    if (!found)
    {
        std::printf("%s has no audio module class\n", path.c_str());
        return 2;
    }

    auto make = [&]() {
        auto inst = std::make_unique<Instance>(factory, classInfo);
        if (!inst->ok)
        {
            std::printf("cannot create an instance of %s\n", classInfo.name().c_str());
            std::exit(2);
        }
        return inst;
    };

    // --- A: the saved session ---------------------------------------------------------------
    // Activation, then the main thread's time to finish what a plug-in starts there (clap-wrapper
    // publishes the preset list once the plug-in's preset crawl is done).
    auto start = [](Instance& inst) {
        if (!inst.activate())
        {
            std::printf("activation failed\n");
            std::exit(2);
        }
        platform::pump(0.5);
    };
    auto a = make();
    start(*a);
    const Catalog cat = catalog(*a);
    int32 program = -1;
    if (!cat.programs.empty() && cat.programParam != kNoParamId)
    {
        program = cat.programs.size() > 1 ? 1 : 0;
        if (!programText.empty()) program = std::atoi(programText.c_str());
        if (!presetName.empty())
        {
            auto it = std::find(cat.programs.begin(), cat.programs.end(), presetName);
            if (it == cat.programs.end())
            {
                std::printf("no program named '%s'\n", presetName.c_str());
                return 2;
            }
            program = int32(it - cat.programs.begin());
        }
        if (program < 0 || program >= int32(cat.programs.size()))
        {
            std::printf("program %d is out of range (0-%zu)\n", program, cat.programs.size() - 1);
            return 2;
        }
    }
    else if (!presetName.empty() || !programText.empty())
    {
        std::printf("the plug-in has no program list\n");
        return 2;
    }
    State fileState;
    if (!stateFile.empty() && !readState(stateFile, fileState, error))
    {
        std::printf("%s\n", error.c_str());
        return 2;
    }

    size_t editable = 0, midi = 0;
    for (auto& p : cat.params)
    {
        editable += cat.editable(p);
        midi += p.midi;
    }
    std::printf("plug-in: %s (%s)\n", classInfo.name().c_str(),
                a->single ? "one object for component and controller" : "separate controller");
    if (a->effect) std::printf("an effect: its main input gets the test signal\n");
    std::printf("parameters: %zu, %zu moved by the edits, %zu MIDI controllers\n", cat.params.size(), editable, midi);
    if (program >= 0) std::printf("program: %d '%s' of %zu\n", program, cat.programs[program].c_str(), cat.programs.size());

    std::vector<std::pair<ParamID, ParamValue>> theEdits;
    // A's history up to the state; replayed exactly by the baseline instance.
    auto history = [&](Instance& inst, bool computeEdits) {
        if (!inst.active) start(inst);
        inst.renderUntil(kProgramAt);
        if (program >= 0) selectProgram(inst, cat, program);
        inst.renderUntil(kEditAt);
        if (!stateFile.empty())
        {
            inst.restoreAsHost(fileState);
            platform::pump(0.3);
        }
        else
        {
            if (computeEdits) theEdits = edits(inst, cat);
            for (auto& e : theEdits) inst.set(e.first, e.second);
        }
        inst.renderUntil(kSaveAt);
    };
    history(*a, true);
    const State saved = a->save();
    const std::vector<double> savedValues = values(*a, cat);
    if (saved.component.empty())
    {
        std::printf("getState gave no component state\n");
        return 2;
    }
    std::printf("state: %zu bytes component, %zu bytes controller\n", saved.component.size(), saved.controller.size());
    if (!stateFile.empty())
        std::printf("  the state from %s, saved again by A: %s\n", stateFile.c_str(),
                    saved.component == fileState.component ? "unchanged"
                                                           : "different bytes (a newer format, or not all of it kept)");
    if (!saveFile.empty() && writeState(saveFile, saved, classInfo.ID()))
        std::printf("saved %s\n", saveFile.c_str());

    // The edits must have reached A's controller (else they cannot be checked after a restore).
    if (stateFile.empty())
    {
        size_t missed = 0;
        for (auto& e : theEdits)
            if (std::abs(a->controller->getParamNormalized(e.first) - e.second) > 1e-6) ++missed;
        if (missed) std::printf("note: %zu of %zu edits do not show in A's controller\n", missed, theEdits.size());
    }

    a->renderUntil(kNotesAt);
    const Audio reference = a->render(kNoteBlocks, kNotes);
    a->render(kOffBlocks, kNotesOff);
    a.reset();
    const auto refBands = bands(reference.first);
    if (!wavDir.empty()) writeWav(wavDir + "/A-reference.wav", reference);

    // --- what can be asked of the plug-in --------------------------------------------------------
    // Bit-identity needs the same sound from the same calls: a second instance with A's history.
    bool exact;
    {
        auto b = make();
        history(*b, false);
        b->save();
        b->renderUntil(kNotesAt);
        Audio again = b->render(kNoteBlocks, kNotes);
        b->render(kOffBlocks, kNotesOff);
        float d = maxDiff(reference, again);
        exact = d <= 1e-5f;
        std::printf("baseline: a second instance with A's history: largest sample difference %.3g, band difference %.2f dB\n",
                    d, bandDiff(refBands, bands(again.first)));
        if (!exact)
            std::printf("  the sound differs per instance (random sources?): restores are compared by spectrum\n");
    }

    // The program alone, without the edits: how much the edits change (a restore that lost
    // them must be told apart), and whether the moment a sound is loaded matters (oscillators
    // that run from the load on). If it does not, a restore at any moment must give A's samples.
    std::vector<double> programValues;
    Audio programSound;
    State programState;
    bool timeFree = false;
    if (program >= 0)
    {
        auto programmed = [&](int64 at, std::vector<double>* shown, State* st) {
            auto u = make();
            start(*u);
            u->renderUntil(at);
            selectProgram(*u, cat, program);
            u->renderUntil(kNotesAt);
            if (shown) *shown = values(*u, cat);
            if (st) *st = u->save();
            Audio out = u->render(kNoteBlocks, kNotes);
            u->render(kOffBlocks, kNotesOff);
            return out;
        };
        programSound = programmed(kProgramAt, &programValues, &programState);
        Audio later = programmed(12, nullptr, nullptr);
        float dl = maxDiff(programSound, later);
        timeFree = exact && dl <= 1e-5f;
        if (stateFile.empty())
        {
            std::printf("the edits change the sound by %.2f dB (bands), largest sample difference %.3g\n",
                        bandDiff(refBands, bands(programSound.first)), maxDiff(reference, programSound));
            if (maxDiff(reference, programSound) <= 1e-5f)
                std::printf("  note: the edits do not change the sound; a stale restore would not be heard\n");
        }
        std::printf("the program loaded 8 blocks later: largest sample difference %.3g (%s)\n", dl,
                    timeFree ? "the load time does not matter: every restore must give A's samples"
                             : "the load time matters: restores are compared by spectrum and with a twin");
    }
    else
        std::printf("no program to test whether the load time matters: restores are compared by spectrum and with a twin\n");

    // --- B: restored ----------------------------------------------------------------------------
    using Restore = std::function<void(Instance&)>;
    struct Scenario
    {
        std::string name;
        Restore restore; // leaves the instance active
        // An instance given the same calls at the same moments, without what must not matter:
        // the two render the same samples (or, with random sources, the same spectrum).
        std::string twinName;
        Restore twin;
        // The expected sound, values and state; the saved session's unless given.
        const Audio* sound = nullptr;
        const std::vector<double>* expect = nullptr;
        const State* state = nullptr;
    };
    int32 other = -1;
    for (int32 i = 0; i < int32(cat.programs.size()); ++i)
        if (i != program && cat.programs[i] != "Init" && cat.programs[i] != "Empty Case")
        {
            other = i;
            break;
        }
    const Restore afterRender = [&](Instance& b) { start(b); b.renderUntil(12); b.setState(saved); };
    std::vector<Scenario> scenarios = {
        {"project load: state after activation and a short render", afterRender},
        {"project load: state right after creation, before setupProcessing",
         [&](Instance& b) { b.setState(saved); start(b); },
         "state right after activation", [&](Instance& b) { start(b); b.setState(saved); }},
        {"state while deactivated, after playing",
         [&](Instance& b) { start(b); b.renderUntil(12); b.deactivate(); b.setState(saved); start(b); },
         "state after the reactivation",
         [&](Instance& b) { start(b); b.renderUntil(12); b.deactivate(); start(b); b.setState(saved); }},
        {"state into an instance that played something else",
         [&](Instance& b) {
             start(b);
             if (other >= 0) selectProgram(b, cat, other);
             b.render(30, {{60, 100.f / 127.f}});
             b.render(30, {{60, 0.f}});
             b.setState(saved);
         }},
        {"state twice (copy, then undo/redo)",
         [&](Instance& b) { start(b); b.renderUntil(8); b.setState(saved); b.render(4); b.setState(saved); }},
        {"state, then the host re-sends every parameter it shows",
         [&](Instance& b) {
             afterRender(b);
             for (auto& p : cat.params)
                 if (cat.shown(p)) b.queue(p.id, b.controller->getParamNormalized(p.id));
         },
         "no re-send", afterRender},
        {"host order: setState, the controller's setComponentState, its setState",
         [&](Instance& b) { start(b); b.renderUntil(12); b.restoreAsHost(saved); },
         "setState alone", afterRender},
    };
    if (program >= 0 && stateFile.empty())
        scenarios.push_back({"program from the host's list (no state)",
                             [&](Instance& b) { start(b); b.renderUntil(12); selectProgram(b, cat, program); },
                             "", nullptr, &programSound, &programValues, &programState});

    struct Result
    {
        Audio sound;
        bool sameHistory = false;
        std::vector<double> shown;
        State resaved;
        std::string viewNote;
    };
    auto run = [&](const Restore& restore, bool withView) {
        Result r;
        auto b = make();
        restore(*b);
        if (injectStale)
            for (auto& p : cat.params)
                if (cat.editable(p)) b->queue(p.id, p.defaultValue);
        platform::pump(0.3);
        if (withView)
        {
            IPtr<IPlugView> view = owned(b->controller->createView(ViewType::kEditor));
            if (!view)
                r.viewNote = "no view";
            else
            {
                auto frame = owned(new Frame);
                if (!platform::showView(view, frame, 2.0, r.viewNote) && r.viewNote.empty())
                    r.viewNote = "the view did not open";
            }
        }
        b->renderUntil(std::max(kNotesAt, b->rendered / kBlock + kSettle));
        r.sameHistory = b->rendered == kNotesAt * kBlock;
        r.shown = values(*b, cat);
        r.resaved = b->save();
        r.sound = b->render(kNoteBlocks, kNotes);
        b->render(kOffBlocks, kNotesOff);
        return r;
    };
    // Two renders that must be the same: samples, or the spectrum where the plug-in has
    // random sources.
    auto alike = [&](const Audio& x, const Audio& y, std::string& text) {
        char buf[96];
        if (exact)
        {
            float d = maxDiff(x, y);
            std::snprintf(buf, sizeof buf, "largest sample difference %.3g", d);
            text = buf;
            return d <= 1e-5f;
        }
        double d = bandDiff(bands(x.first), bands(y.first));
        std::snprintf(buf, sizeof buf, "band difference %.2f dB", d);
        text = buf;
        return d < 0.2;
    };
    auto staleValues = [&](const std::vector<double>& shown, const std::vector<double>& expect) {
        std::vector<std::string> stale;
        for (size_t i = 0; i < cat.params.size(); ++i)
        {
            if (!cat.stateful(cat.params[i]) || std::abs(shown[i] - expect[i]) <= 1e-6) continue;
            char buf[160];
            std::snprintf(buf, sizeof buf, "%s %.4f (saved %.4f)", cat.params[i].name.c_str(), shown[i], expect[i]);
            stale.push_back(buf);
        }
        return stale;
    };
    size_t compared = 0;
    for (auto& p : cat.params) compared += cat.stateful(p);

    int failures = 0;
    int index = 0;
    for (auto& s : scenarios)
    {
        ++index;
        const Audio& expected = s.sound ? *s.sound : reference;
        const std::vector<double>& expectValues = s.expect ? *s.expect : savedValues;
        const State& expectState = s.state ? *s.state : saved;
        const auto expectBands = bands(expected.first);
        Result b = run(s.restore, false);
        std::vector<std::string> problems;

        // The sound, against the saved session's.
        std::string text;
        char buf[200];
        bool audible = rms(b.sound.first, size_t(0.1 * kRate)) > 1e-3;
        if (!audible) problems.push_back("silent");
        if (timeFree && b.sameHistory)
        {
            float d = maxDiff(expected, b.sound);
            std::snprintf(buf, sizeof buf, "largest sample difference %.3g", d);
            if (d > 1e-5f) problems.push_back("not the saved sound");
        }
        else
        {
            double d = bandDiff(expectBands, bands(b.sound.first));
            std::snprintf(buf, sizeof buf, "band difference %.2f dB", d);
            if (d >= 1.5) problems.push_back("not the saved sound");
        }
        text = buf;

        // The values the controller shows, and the processor's (its state, taken again).
        auto stale = staleValues(b.shown, expectValues);
        bool sameState = b.resaved.component == expectState.component;
        std::snprintf(buf, sizeof buf, ", %zu of %zu values stale, state %s", stale.size(), compared,
                      sameState ? "saved again unchanged" : "saved again DIFFERS");
        text += buf;
        if (!stale.empty()) problems.push_back("stale values");
        if (!sameState) problems.push_back("the processor's state differs from the restored one");

        if (s.twin)
        {
            Result t = run(s.twin, false);
            std::string d;
            bool same = alike(b.sound, t.sound, d);
            text += "; vs " + s.twinName + ": " + d;
            if (!same) problems.push_back("differs from " + s.twinName);
        }
        if (view)
        {
            Result e = run(s.restore, true);
            if (!e.viewNote.empty())
            {
                text += "; view: " + e.viewNote;
                problems.push_back("view: " + e.viewNote);
            }
            else
            {
                std::string d;
                bool same = alike(b.sound, e.sound, d);
                auto staleOpened = staleValues(e.shown, expectValues);
                text += "; view closed/opened: " + d;
                if (!staleOpened.empty()) text += ", " + std::to_string(staleOpened.size()) + " stale with the view";
                if (!same) problems.push_back("the view changes the sound");
                if (!staleOpened.empty()) problems.push_back("stale values with the view");
            }
            if (!wavDir.empty())
                writeWav(wavDir + "/B" + std::to_string(index) + "-" + slug(s.name) + "-view.wav", e.sound);
        }
        if (!wavDir.empty()) writeWav(wavDir + "/B" + std::to_string(index) + "-" + slug(s.name) + ".wav", b.sound);

        bool ok = problems.empty();
        if (!ok) ++failures;
        std::printf("%s  %s: %s\n", ok ? "ok  " : "FAIL", s.name.c_str(), text.c_str());
        if (!ok)
        {
            std::string all;
            for (auto& p : problems) all += (all.empty() ? "" : "; ") + p;
            std::printf("      %s\n", all.c_str());
            for (size_t i = 0; i < stale.size() && i < 6; ++i) std::printf("      stale: %s\n", stale[i].c_str());
            if (stale.size() > 6) std::printf("      stale: %zu more\n", stale.size() - 6);
            std::printf("      restored %s\n", bandLine(bands(b.sound.first)).c_str());
            std::printf("      saved    %s\n", bandLine(expectBands).c_str());
        }
    }

    std::printf(failures == 0 ? "OK: every restored session sounds as saved and shows its values\n"
                              : "FAIL: %d scenario(s) lost the saved sound or show stale values\n",
                failures);
    return failures == 0 ? 0 : 1;
}
