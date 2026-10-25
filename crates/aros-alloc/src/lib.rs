// SPDX-License-Identifier: MIT
// Copyright (c) 2026 John Knipper

//! A size-class allocator in front of the system allocator, for native AROS.
//!
//! On AROS, `malloc` is exec's pooled allocator behind a semaphore and
//! Forbid/Permit; on AArch64 AROS every Permit runs a supervisor check
//! and a scheduler pass, so a small allocation costs microseconds and a
//! kernel round trip. GPUI's layout allocates thousands of small objects per
//! frame; a profile of Ferail's UI thread on AROS put two thirds of its
//! busy time in that path.
//!
//! Requests up to [`MAX_SMALL`] bytes with alignment up to 16 are served
//! from per-class free lists refilled from slabs the system allocator hands
//! out [`SLAB`] bytes at a time; freed blocks return to their class and are
//! reused, never given back to the system (a long-lived GUI process keeps
//! its working set). Larger or more aligned requests go to the system
//! allocator unchanged. One spin lock per class keeps the lists correct
//! across threads; the critical sections are a few instructions.
//!
//! Every platform gets the same allocator so the host tests exercise it;
//! only AROS pays the system allocator's price that justifies it.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::UnsafeCell;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Largest request served from the classes, in bytes.
pub const MAX_SMALL: usize = 2048;
/// Bytes taken from the system allocator per refill.
pub const SLAB: usize = 256 * 1024;
/// Alignment every class block satisfies.
const CLASS_ALIGN: usize = 16;

const CLASSES: [usize; 16] = [
    16, 32, 48, 64, 96, 128, 192, 256, 320, 384, 512, 768, 1024, 1280, 1536, 2048,
];

fn class_index(size: usize) -> Option<usize> {
    if size > MAX_SMALL {
        return None;
    }
    CLASSES.iter().position(|&c| c >= size)
}

struct FreeList {
    lock: AtomicBool,
    head: UnsafeCell<*mut u8>,
}

// SAFETY: `head` is only touched while `lock` is held, from any thread.
unsafe impl Sync for FreeList {}
unsafe impl Send for FreeList {}

impl FreeList {
    const fn new() -> Self {
        Self { lock: AtomicBool::new(false), head: UnsafeCell::new(ptr::null_mut()) }
    }

    fn acquire(&self) {
        while self
            .lock
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            std::hint::spin_loop();
        }
    }

    fn release(&self) {
        self.lock.store(false, Ordering::Release);
    }
}

/// The allocator. One instance is the `#[global_allocator]` of a binary.
pub struct ArosAlloc {
    lists: [FreeList; 16],
    /// Bytes taken from the system allocator for slabs (diagnostics).
    slab_bytes: AtomicUsize,
    /// Allocations served from the classes (diagnostics).
    small_allocs: AtomicUsize,
}

impl ArosAlloc {
    pub const fn new() -> Self {
        const LIST: FreeList = FreeList::new();
        Self { lists: [LIST; 16], slab_bytes: AtomicUsize::new(0), small_allocs: AtomicUsize::new(0) }
    }

    /// Bytes of slabs taken from the system so far.
    pub fn slab_bytes(&self) -> usize {
        self.slab_bytes.load(Ordering::Relaxed)
    }

    /// Allocations served from the size classes so far.
    pub fn small_allocs(&self) -> usize {
        self.small_allocs.load(Ordering::Relaxed)
    }

    /// Carve a fresh slab into blocks of `size` and push them on the list
    /// (the list is held). Returns false when the system allocator failed.
    unsafe fn refill(&self, list: &FreeList, size: usize) -> bool {
        let layout = Layout::from_size_align_unchecked(SLAB, CLASS_ALIGN);
        let slab = System.alloc(layout);
        if slab.is_null() {
            return false;
        }
        self.slab_bytes.fetch_add(SLAB, Ordering::Relaxed);
        let count = SLAB / size;
        let head = list.head.get();
        // Thread the blocks in address order; the first block becomes the head.
        let mut i = count;
        while i > 0 {
            i -= 1;
            let block = slab.add(i * size);
            ptr::write(block as *mut *mut u8, *head);
            *head = block;
        }
        true
    }
}

impl Default for ArosAlloc {
    fn default() -> Self {
        Self::new()
    }
}

unsafe impl GlobalAlloc for ArosAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if layout.align() > CLASS_ALIGN {
            return System.alloc(layout);
        }
        let Some(index) = class_index(layout.size().max(1)) else {
            return System.alloc(layout);
        };
        let list = &self.lists[index];
        list.acquire();
        let head = list.head.get();
        if (*head).is_null() && !self.refill(list, CLASSES[index]) {
            list.release();
            return ptr::null_mut();
        }
        let block = *head;
        *head = ptr::read(block as *const *mut u8);
        list.release();
        self.small_allocs.fetch_add(1, Ordering::Relaxed);
        block
    }

    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        if layout.align() > CLASS_ALIGN {
            return System.dealloc(p, layout);
        }
        let Some(index) = class_index(layout.size().max(1)) else {
            return System.dealloc(p, layout);
        };
        let list = &self.lists[index];
        list.acquire();
        let head = list.head.get();
        ptr::write(p as *mut *mut u8, *head);
        *head = p;
        list.release();
    }

    unsafe fn realloc(&self, p: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // Same class: the block already fits.
        if layout.align() <= CLASS_ALIGN {
            if let (Some(old), Some(new)) = (class_index(layout.size().max(1)), class_index(new_size.max(1))) {
                if old == new {
                    return p;
                }
            }
        }
        let new_layout = Layout::from_size_align_unchecked(new_size, layout.align());
        let q = self.alloc(new_layout);
        if !q.is_null() {
            ptr::copy_nonoverlapping(p, q, layout.size().min(new_size));
            self.dealloc(p, layout);
        }
        q
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes_cover_every_small_size() {
        for size in 1..=MAX_SMALL {
            let i = class_index(size).expect("a class");
            assert!(CLASSES[i] >= size);
            assert!(i == 0 || CLASSES[i - 1] < size);
        }
        assert_eq!(class_index(MAX_SMALL + 1), None);
    }

    #[test]
    fn blocks_are_reused_and_distinct() {
        let a = ArosAlloc::new();
        let layout = Layout::from_size_align(40, 8).unwrap();
        unsafe {
            let p = a.alloc(layout);
            let q = a.alloc(layout);
            assert_ne!(p, q);
            assert_eq!(p as usize % CLASS_ALIGN, 0);
            ptr::write_bytes(p, 0xaa, 40);
            ptr::write_bytes(q, 0x55, 40);
            a.dealloc(p, layout);
            let r = a.alloc(layout);
            assert_eq!(r, p, "the freed block is reused first");
            a.dealloc(q, layout);
            a.dealloc(r, layout);
        }
        assert_eq!(a.slab_bytes(), SLAB);
        assert_eq!(a.small_allocs(), 3);
    }

    #[test]
    fn realloc_keeps_the_block_within_a_class_and_moves_across() {
        let a = ArosAlloc::new();
        let layout = Layout::from_size_align(20, 8).unwrap();
        unsafe {
            let p = a.alloc(layout);
            ptr::write_bytes(p, 0x11, 20);
            let same = a.realloc(p, layout, 30);
            assert_eq!(same, p);
            let bigger = a.realloc(same, Layout::from_size_align(30, 8).unwrap(), 4000);
            assert_ne!(bigger, p);
            assert_eq!(*bigger, 0x11);
            a.dealloc(bigger, Layout::from_size_align(4000, 8).unwrap());
        }
    }

    #[test]
    fn large_and_overaligned_requests_bypass_the_classes() {
        let a = ArosAlloc::new();
        unsafe {
            let big = Layout::from_size_align(MAX_SMALL + 1, 8).unwrap();
            let p = a.alloc(big);
            assert!(!p.is_null());
            a.dealloc(p, big);
            let aligned = Layout::from_size_align(64, 64).unwrap();
            let q = a.alloc(aligned);
            assert_eq!(q as usize % 64, 0);
            a.dealloc(q, aligned);
        }
        assert_eq!(a.small_allocs(), 0);
    }

    #[test]
    fn many_threads_share_the_lists() {
        use std::sync::Arc;
        let a = Arc::new(ArosAlloc::new());
        let handles: Vec<_> = (0..8)
            .map(|t| {
                let a = a.clone();
                std::thread::spawn(move || {
                    let layout = Layout::from_size_align(16 + t * 8, 8).unwrap();
                    // Addresses as integers: raw pointers are not Send.
                    let mut held: Vec<usize> = Vec::new();
                    for i in 0..2000 {
                        let p = unsafe { a.alloc(layout) };
                        assert!(!p.is_null());
                        unsafe { *p = i as u8 };
                        held.push(p as usize);
                        if held.len() > 50 {
                            let q = held.remove(0);
                            unsafe { a.dealloc(q as *mut u8, layout) };
                        }
                    }
                    for p in held {
                        unsafe { a.dealloc(p as *mut u8, layout) };
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
    }
}
