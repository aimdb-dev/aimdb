#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use core::hint::black_box;
use core::mem::MaybeUninit;
use core::panic::PanicInfo;

use ciborium::Value;
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
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(black_box(&fixture), &mut bytes).expect("trusted CBOR fixture");
        let dynamic: Value =
            ciborium::de::from_reader(black_box(bytes.as_slice())).expect("self-encoded CBOR");
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
