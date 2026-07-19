#![no_std]
#![no_main]

extern crate alloc;

use core::hint::black_box;
use core::mem::MaybeUninit;
use core::panic::PanicInfo;

use cortex_m as _;
use cortex_m_rt::entry;
use embedded_alloc::LlffHeap;
use remote_representation_footprint::sample;

#[global_allocator]
static HEAP: LlffHeap = LlffHeap::empty();

#[entry]
fn main() -> ! {
    init_heap();
    let fixture = sample();
    loop {
        let bytes = serde_json::to_vec(black_box(&fixture)).expect("trusted JSON fixture");
        let dynamic: serde_json::Value =
            serde_json::from_slice(black_box(&bytes)).expect("self-encoded JSON");
        black_box(dynamic);
    }
}

fn init_heap() {
    const HEAP_BYTES: usize = 32 * 1024;
    static mut MEMORY: [MaybeUninit<u8>; HEAP_BYTES] = [MaybeUninit::uninit(); HEAP_BYTES];
    // SAFETY: called once before any allocation; the region is static and is
    // owned exclusively by the global allocator for the rest of the program.
    unsafe {
        let memory = core::ptr::addr_of_mut!(MEMORY);
        HEAP.init((*memory).as_ptr() as usize, HEAP_BYTES);
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
