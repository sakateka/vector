use std::{
    alloc::{GlobalAlloc, Layout},
    cell::Cell,
    ptr,
    sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering},
};

use super::{
    token::{AllocationGroupId, try_with_suspended_allocation_group},
    tracer::Tracer,
};
use crate::internal_telemetry::allocations::TRACK_ALLOCATIONS;

// Nodes remain registered after thread exit: another thread can free their allocations.
#[repr(align(64))]
struct ThreadBytes {
    locked: AtomicBool,
    bytes: AtomicUsize,
    next: AtomicPtr<ThreadBytes>,
}

impl ThreadBytes {
    const fn new(bytes: usize, next: *mut Self) -> Self {
        Self {
            locked: AtomicBool::new(false),
            bytes: AtomicUsize::new(bytes),
            next: AtomicPtr::new(next),
        }
    }

    fn lock(&self) {
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            std::hint::spin_loop();
        }
    }

    fn unlock(&self) {
        self.locked.store(false, Ordering::Release);
    }

    fn add(&self, bytes: usize) {
        self.lock();
        self.bytes.store(
            self.bytes.load(Ordering::Relaxed).wrapping_add(bytes),
            Ordering::Relaxed,
        );
        self.unlock();
    }
}

thread_local! {
    // No allocation or destructor: this remains usable during other TLS destructors.
    static THREAD_BYTES: Cell<*mut ThreadBytes> = const { Cell::new(ptr::null_mut()) };
}

/// A tracing allocator that groups allocation events by groups.
///
/// This allocator can only be used when specified via `#[global_allocator]`.
pub struct GroupedTraceableAllocator<A, T> {
    allocator: A,
    tracer: T,
    threads: AtomicPtr<ThreadBytes>,
    registry: ThreadBytes,
}

impl<A, T> GroupedTraceableAllocator<A, T> {
    /// Creates a new `GroupedTraceableAllocator` that wraps the given allocator and tracer.
    #[must_use]
    pub const fn new(allocator: A, tracer: T) -> Self {
        Self {
            allocator,
            tracer,
            threads: AtomicPtr::new(ptr::null_mut()),
            registry: ThreadBytes::new(0, ptr::null_mut()),
        }
    }

    /// Returns a coherent total of live bytes requested from the underlying allocator.
    /// Includes tracking metadata, but excludes allocator caches and size-class rounding.
    pub fn live_bytes(&self) -> usize {
        // Freeze registration, then all thread counters in list order. Updates only lock one.
        self.registry.lock();
        let head = self.threads.load(Ordering::Relaxed);
        let mut current = head;
        let mut total = self.registry.bytes.load(Ordering::Relaxed);
        // SAFETY: The registry contains initialized nodes retained for the process lifetime.
        while let Some(counter) = unsafe { current.as_ref() } {
            counter.lock();
            total = total.wrapping_add(counter.bytes.load(Ordering::Relaxed));
            current = counter.next.load(Ordering::Relaxed);
        }
        current = head;
        // SAFETY: The registry contains initialized nodes retained for the process lifetime.
        while let Some(counter) = unsafe { current.as_ref() } {
            counter.unlock();
            current = counter.next.load(Ordering::Relaxed);
        }
        self.registry.unlock();
        total
    }
}

impl<A: GlobalAlloc, T> GroupedTraceableAllocator<A, T> {
    fn record_bytes(&self, bytes: usize) {
        let recorded = THREAD_BYTES.try_with(|slot| {
            let mut pointer = slot.get();
            if pointer.is_null() {
                // Bypass the wrapper to avoid recursive TLS initialization. One cache line per
                // thread is retained for the process lifetime, matching the tracing registry.
                let layout = Layout::new::<ThreadBytes>();
                // SAFETY: The nonzero layout describes the node initialized below.
                pointer = unsafe { self.allocator.alloc(layout).cast::<ThreadBytes>() };
                if pointer.is_null() {
                    // Accounting must continue even when allocating bookkeeping fails.
                    self.registry.add(bytes);
                    return;
                }
                self.registry.lock();
                // SAFETY: Allocation succeeded with the required node size and alignment.
                unsafe {
                    pointer.write(ThreadBytes::new(
                        layout.size(),
                        self.threads.load(Ordering::Relaxed),
                    ));
                }
                self.threads.store(pointer, Ordering::Relaxed);
                self.registry.unlock();
                slot.set(pointer);
            }
            // SAFETY: Registered counters are initialized before use and never freed.
            // Wrapping deltas allow allocation and deallocation on different threads.
            unsafe { &*pointer }.add(bytes);
        });
        if recorded.is_err() {
            self.registry.add(bytes);
        }
    }
}

// SAFETY: Allocation and deallocation are delegated to `A` with matching layouts.
unsafe impl<A: GlobalAlloc, T: Tracer> GlobalAlloc for GroupedTraceableAllocator<A, T> {
    #[inline]
    unsafe fn alloc(&self, object_layout: Layout) -> *mut u8 {
        // SAFETY: The caller upholds `GlobalAlloc::alloc`'s contract.
        unsafe {
            if !TRACK_ALLOCATIONS.load(Ordering::Relaxed) {
                let ptr = self.allocator.alloc(object_layout);
                if !ptr.is_null() {
                    self.record_bytes(object_layout.size());
                }
                return ptr;
            }

            // Allocate our wrapped layout and make sure the allocation succeeded.
            let (actual_layout, offset_to_group_id) = get_wrapped_layout(object_layout);
            let actual_ptr = self.allocator.alloc(actual_layout);
            if actual_ptr.is_null() {
                return actual_ptr;
            }
            self.record_bytes(actual_layout.size());

            let group_id_ptr = actual_ptr.add(offset_to_group_id).cast::<u8>();

            let object_size = actual_layout.size();

            try_with_suspended_allocation_group(
                #[inline(always)]
                |group_id| {
                    group_id_ptr.write(group_id.as_raw());
                    self.tracer.trace_allocation(object_size, group_id);
                },
            );
            actual_ptr
        }
    }

    #[inline]
    unsafe fn dealloc(&self, object_ptr: *mut u8, object_layout: Layout) {
        // SAFETY: The caller provides a pointer and layout returned by `alloc`.
        unsafe {
            if !TRACK_ALLOCATIONS.load(Ordering::Relaxed) {
                self.allocator.dealloc(object_ptr, object_layout);
                self.record_bytes(object_layout.size().wrapping_neg());
                return;
            }
            // Regenerate the wrapped layout so we know where we have to look, as the pointer we've given relates to the
            // requested layout, not the wrapped layout that was actually allocated.
            let (wrapped_layout, offset_to_group_id) = get_wrapped_layout(object_layout);

            let raw_group_id = object_ptr.add(offset_to_group_id).cast::<u8>().read();

            // Deallocate before tracking, just to make sure we're reclaiming memory as soon as possible.
            self.allocator.dealloc(object_ptr, wrapped_layout);
            self.record_bytes(wrapped_layout.size().wrapping_neg());

            let object_size = wrapped_layout.size();
            let source_group_id = AllocationGroupId::from_raw(raw_group_id);

            try_with_suspended_allocation_group(
                #[inline(always)]
                |_| {
                    self.tracer.trace_deallocation(object_size, source_group_id);
                },
            );
        }
    }
}

#[inline(always)]
fn get_wrapped_layout(object_layout: Layout) -> (Layout, usize) {
    static HEADER_LAYOUT: Layout = Layout::new::<u8>();

    // We generate a new allocation layout that gives us a location to store the active allocation group ID ahead
    // of the requested allocation, which lets us always attempt to retrieve it on the deallocation path.
    let (actual_layout, offset_to_group_id) = object_layout
        .extend(HEADER_LAYOUT)
        .expect("wrapping requested layout resulted in overflow");

    (actual_layout.pad_to_align(), offset_to_group_id)
}
