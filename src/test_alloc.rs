//! The one counting allocator for tests. A test reads the allocations of its own thread only,
//! so allocation-count tests in different modules do not see each other's.

use std::{alloc::{GlobalAlloc, Layout, System}, cell::Cell};

struct Counting;

thread_local! {
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
    /// Bytes asked for in all, and bytes live now (allocated minus freed), on this thread.
    static BYTES: Cell<usize> = const { Cell::new(0) };
    static LIVE: Cell<isize> = const { Cell::new(0) };
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.with(|count| count.set(count.get() + 1));
        BYTES.with(|bytes| bytes.set(bytes.get() + layout.size()));
        LIVE.with(|live| live.set(live.get() + layout.size() as isize));
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.with(|live| live.set(live.get() - layout.size() as isize));
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

/// Runs `run` and returns its result with the allocations this thread made meanwhile.
pub fn allocations<T>(run: impl FnOnce() -> T) -> (T, usize) {
    let before = ALLOCS.with(Cell::get);
    let out = run();
    (out, ALLOCS.with(Cell::get) - before)
}

/// Runs `run` and returns its result with the bytes this thread asked the allocator for meanwhile.
pub fn bytes<T>(run: impl FnOnce() -> T) -> (T, usize) {
    let before = BYTES.with(Cell::get);
    let out = run();
    (out, BYTES.with(Cell::get) - before)
}

/// Runs `run` and returns its result with the bytes still live once it has returned (the result's own included).
pub fn held<T>(run: impl FnOnce() -> T) -> (T, isize) {
    let before = LIVE.with(Cell::get);
    let out = run();
    (out, LIVE.with(Cell::get) - before)
}
