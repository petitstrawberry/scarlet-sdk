//! POSIX pipes backed by Native pipe handles and Selectable controls.

use scarlet_abi::{ERRNO_EINVAL, ERRNO_EIO, SCTL_SOCKET_SET_NONBLOCK, Syscall, fs::ERRNO_EFAULT};
use std::ffi::c_int;

const O_NONBLOCK: c_int = 0x800;
const O_CLOEXEC: c_int = 0x80000;

fn result(value: usize) -> Result<usize, c_int> {
    if value == usize::MAX {
        Err(ERRNO_EIO)
    } else if value > isize::MAX as usize {
        Err((value as isize).wrapping_neg() as c_int)
    } else {
        Ok(value)
    }
}

/// # Safety
/// `fds` must have room for two C integers. It is unchanged on failure.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pipe2(fds: *mut c_int, flags: c_int) -> c_int {
    if fds.is_null() {
        return crate::fail(ERRNO_EFAULT);
    }
    if flags & !(O_NONBLOCK | O_CLOEXEC) != 0 {
        return crate::fail(ERRNO_EINVAL);
    }
    let mut handles = [0u32; 2];
    // SAFETY: Native Pipe writes exactly two u32 handles to this local array.
    let created = unsafe { scarlet_sys::syscall2(Syscall::Pipe, handles.as_mut_ptr() as usize, 0) };
    if let Err(error) = result(created) {
        return crate::fail(error);
    }
    let setup = (|| {
        for handle in handles {
            if handle > c_int::MAX as u32 {
                return Err(ERRNO_EIO);
            }
            if flags & O_CLOEXEC != 0 {
                // SAFETY: this newly owned descriptor accepts scalar CLOEXEC flags.
                result(unsafe {
                    scarlet_sys::syscall2(Syscall::HandleSetDescriptorFlags, handle as usize, 1)
                })?;
            }
            if flags & O_NONBLOCK != 0 {
                // The legacy socket-named opcode is a generic Selectable control.
                // SAFETY: only the new pipe endpoint's nonblocking state is changed.
                result(unsafe {
                    scarlet_sys::syscall3(
                        Syscall::HandleControl,
                        handle as usize,
                        SCTL_SOCKET_SET_NONBLOCK as usize,
                        1,
                    )
                })?;
            }
        }
        Ok(())
    })();
    if let Err(error) = setup {
        for handle in handles {
            // SAFETY: both handles were created by this call and not published.
            unsafe { scarlet_sys::syscall1(Syscall::HandleClose, handle as usize) };
        }
        return crate::fail(error);
    }
    // SAFETY: the caller provides an exclusive two-element output array.
    unsafe {
        fds.write(handles[0] as c_int);
        fds.add(1).write(handles[1] as c_int);
    }
    0
}

/// # Safety
/// `fds` must have room for two C integers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pipe(fds: *mut c_int) -> c_int {
    // SAFETY: this is the same output contract as pipe2, without extra flags.
    unsafe { pipe2(fds, 0) }
}
