# What is real-time safe

A host asks your plugin for audio many times a second: 256 samples at 48 kHz is a request every
5.3 milliseconds, and the answer must be ready in a fraction of that time, every time. If one
block arrives late the listener hears a click or a dropout. The thread that does this is the
*audio thread*, and a few rules keep it on time.

## The rules

On the audio thread (`process`, `event`, `reset`), **never**:

- **allocate or free memory** (`Vec::new` followed by a push, `Box::new`, `String`, `format!`,
  `vec![...]`, cloning anything that owns heap memory). The allocator may take a lock or ask
  the operating system for memory, and neither has a time limit.
- **lock** a `Mutex` or `RwLock` that another thread may hold. If the other thread is
  descheduled while holding it, the audio thread waits as long as that takes.
- **wait, sleep, do file or network I/O, or print.**
- **call anything that does the above** without knowing. This is the hard part: a library
  function that "just works" may allocate.

What is fine: arithmetic, arrays that already exist, atomics, `Copy` structs, loops, reading
parameters, and the helpers in `apf::dsp`.

## How to live by the rules

**Allocate before audio starts.** `Plugin::new` and `Plugin::activate` run on the main thread
and may allocate. A delay line needs its memory? Create it there (`DelayLine::new`). `activate`
is told the largest block the host will use.

**Use fixed-size storage for voices.** Example 04 keeps `[Voice; 8]`, a plain array of `Copy`
structs. Starting a note overwrites a slot. When all slots are busy a note *steals* one;
nothing grows.

**Share numbers with atomics.** Another thread (the web view, a controller) hands the audio
thread a value through an `AtomicU32` or `AtomicF64`; the audio thread hands one back the same
way. Example 07's meter is the pattern.

**Send events through a queue that cannot block.** `apf::queue::SpscQueue` is a bounded
single-producer single-consumer queue. A full queue refuses the push instead of waiting, and
the sender decides what to drop. The framework's own web view-to-audio path is one.

**Watch for hidden allocation.** `format!`, `.to_string()`, `Vec::push` beyond capacity,
`Box<dyn Trait>` creation, `HashMap` growth, `String` building, and logging macros all
allocate. If you need to tell the user something, set an atomic flag and let the main thread
report it.

## Panics

A panic in `process` must not take down the host. APF catches it at the C boundary, mutes the
instance and reports an error. That is a safety net, not a design: an unwinding panic also
allocates.

## Denormals

Floating point numbers very close to zero (below about 1e-38) are *denormal*, and many CPUs
handle them hundreds of times slower. A reverb tail or a filter decaying to silence passes through
them. APF turns the CPU's flush-to-zero mode on for the duration of every `process` call and
restores the host's setting afterwards, so inside a plugin this is mostly taken care of. See
example 06 for what to do where you need an exact zero.

## Proving it: the counting allocator

Rules that nobody checks are forgotten. Every example has a test that installs a *counting
allocator*, plays notes and automation through the plugin, and fails if `process` allocated or
freed anything:

```rust,no_run
# extern crate apf_test;
#[global_allocator]
static ALLOCATOR: apf_test::CountingAllocator = apf_test::CountingAllocator;

# fn main() {
// ... play the plugin with Instance::process ...
assert_eq!(apf_test::allocations_in_process(), 0, "process() allocated");
# }
```

The allocator counts only while the plugin's `process` is running on the test's thread, so
setup code and the test's own buffers are not blamed. See [Testing without a host](testing.md).
Copy the test into your own plugin; it will save you from the day a harmless-looking change adds
a `Vec`.
