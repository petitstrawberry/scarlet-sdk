//! Typed C access to the existing versioned Native network snapshots.
use scarlet_abi::network::NetworkLinkInfoV1;
use scarlet_abi::{ERRNO_EINVAL, ERRNO_EIO, Syscall};

#[repr(C)]
pub struct Ipv4InfoV2 {
    name: [u8; 32],
    address: [u8; 4],
    netmask: [u8; 4],
    gateway: [u8; 4],
    mac_address: [u8; 6],
    has_address: u8,
    has_gateway: u8,
    is_default: u8,
    reserved: [u8; 3],
    metric: u32,
}
const _: () = {
    assert!(size_of::<NetworkLinkInfoV1>() == 64);
    assert!(size_of::<Ipv4InfoV2>() == 60);
};

fn decode(value: usize) -> isize {
    if value == usize::MAX {
        crate::fail(ERRNO_EIO) as isize
    } else if value > isize::MAX as usize {
        crate::fail((value as isize).wrapping_neg() as i32) as isize
    } else {
        value as isize
    }
}

/// # Safety
/// Non-null output must hold capacity records; NULL with zero capacity probes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn scarlet_network_list_links_v1(
    output: *mut NetworkLinkInfoV1,
    capacity: usize,
) -> isize {
    if capacity > isize::MAX as usize / size_of::<NetworkLinkInfoV1>() {
        return crate::fail(ERRNO_EINVAL) as isize;
    }
    if capacity != 0 && output.is_null() {
        return crate::fail(scarlet_abi::fs::ERRNO_EFAULT) as isize;
    }
    // SAFETY: the caller supplies a writable array and the kernel checks capacity.
    decode(unsafe { scarlet_sys::syscall2(Syscall::NetworkListLinksV1, output as usize, capacity) })
}

/// # Safety
/// Output must hold capacity records. Capacity must be nonzero.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn scarlet_network_list_ipv4_v2(
    output: *mut Ipv4InfoV2,
    capacity: usize,
) -> isize {
    if capacity == 0 || capacity > isize::MAX as usize / size_of::<Ipv4InfoV2>() {
        return crate::fail(ERRNO_EINVAL) as isize;
    }
    if output.is_null() {
        return crate::fail(scarlet_abi::fs::ERRNO_EFAULT) as isize;
    }
    // SAFETY: the caller provides exactly the fixed-width Native record layout.
    decode(unsafe {
        scarlet_sys::syscall2(Syscall::NetworkListInterfacesV2, output as usize, capacity)
    })
}
