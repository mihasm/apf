//! A bounded, wait-free single-producer/single-consumer queue for `Copy` messages between a
//! non-real-time thread and the audio thread.
//!
//! All memory is allocated in [`SpscQueue::new`]; `push` and `pop` never allocate, lock or
//! block. The type is `Sync` so it can sit in shared state, but the contract is that at any
//! moment at most one thread pushes and at most one thread pops. In this framework the view
//! (main thread) is the only producer and the consumer is `process`/`flush`, which CLAP never
//! runs concurrently.

use std::cell::UnsafeCell;
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicUsize, Ordering};

#[repr(align(128))]
struct Padded(AtomicUsize);

/// A wait-free queue of `Copy` values from one producer thread to one consumer thread; a full queue refuses instead of waiting.
pub struct SpscQueue<T: Copy> {
    slots: Box<[UnsafeCell<MaybeUninit<T>>]>,
    mask: usize,
    /// Next slot to read; written only by the consumer.
    head: Padded,
    /// Next slot to write; written only by the producer.
    tail: Padded,
}

unsafe impl<T: Copy + Send> Send for SpscQueue<T> {}
unsafe impl<T: Copy + Send> Sync for SpscQueue<T> {}

impl<T: Copy> SpscQueue<T> {
    /// A queue holding at least `capacity` messages (rounded up to a power of two).
    pub fn new(capacity: usize) -> Self {
        let cap = capacity.max(2).next_power_of_two();
        let slots = (0..cap).map(|_| UnsafeCell::new(MaybeUninit::uninit())).collect();
        Self { slots, mask: cap - 1, head: Padded(AtomicUsize::new(0)), tail: Padded(AtomicUsize::new(0)) }
    }

    /// How many values fit.
    pub fn capacity(&self) -> usize {
        self.mask + 1
    }

    /// Producer side. Returns `false` (and drops nothing else) when the queue is full.
    pub fn push(&self, value: T) -> bool {
        let tail = self.tail.0.load(Ordering::Relaxed);
        let head = self.head.0.load(Ordering::Acquire);
        if tail.wrapping_sub(head) > self.mask {
            return false;
        }
        // SAFETY: only the producer writes this slot, and the consumer will not read it until
        // the Release store below publishes it.
        unsafe { (*self.slots[tail & self.mask].get()).write(value) };
        self.tail.0.store(tail.wrapping_add(1), Ordering::Release);
        true
    }

    /// Consumer side.
    pub fn pop(&self) -> Option<T> {
        let head = self.head.0.load(Ordering::Relaxed);
        let tail = self.tail.0.load(Ordering::Acquire);
        if head == tail {
            return None;
        }
        // SAFETY: the slot was initialised by `push` before its Release store of `tail`.
        let value = unsafe { (*self.slots[head & self.mask].get()).assume_init() };
        self.head.0.store(head.wrapping_add(1), Ordering::Release);
        Some(value)
    }

    /// Whether nothing is waiting.
    pub fn is_empty(&self) -> bool {
        self.head.0.load(Ordering::Acquire) == self.tail.0.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn fills_and_drains_in_order() {
        let q = SpscQueue::new(3);
        assert_eq!(q.capacity(), 4);
        for i in 0..4 {
            assert!(q.push(i));
        }
        assert!(!q.push(99));
        for i in 0..4 {
            assert_eq!(q.pop(), Some(i));
        }
        assert_eq!(q.pop(), None);
    }

    #[test]
    fn two_threads_lose_and_reorder_nothing() {
        const N: u64 = 1_000_000;
        let q = Arc::new(SpscQueue::<u64>::new(64));
        let producer = {
            let q = q.clone();
            std::thread::spawn(move || {
                for i in 0..N {
                    while !q.push(i) {
                        std::hint::spin_loop();
                    }
                }
            })
        };
        let mut expected = 0;
        while expected < N {
            if let Some(v) = q.pop() {
                assert_eq!(v, expected);
                expected += 1;
            }
        }
        producer.join().unwrap();
        assert!(q.is_empty());
    }
}
