//! Integer IEEE exponents, with C domain-error reporting for zero/NaN/infinity.
use crate::ERRNO_EDOM as EDOM;

fn invalid() {
    unsafe {
        crate::__errno_location().write(EDOM);
    }
    // Preserve the other per-thread IEEE flags. AArch64 IOC is bit 0;
    // RISC-V's NV is bit 4. No rounding-mode or FP control state changes.
    #[cfg(target_arch = "aarch64")]
    unsafe {
        core::arch::asm!("mrs {flags}, fpsr", "orr {flags}, {flags}, #1", "msr fpsr, {flags}", flags = out(reg) _, options(nostack));
    }
    #[cfg(target_arch = "riscv64")]
    unsafe {
        core::arch::asm!("csrsi fflags, 16", options(nostack));
    }
}

#[cfg_attr(target_os = "scarlet", unsafe(no_mangle))]
pub extern "C" fn ilogbf(value: f32) -> i32 {
    if value == 0.0 || !value.is_finite() {
        invalid();
    }
    libm::ilogbf(value)
}

#[cfg_attr(target_os = "scarlet", unsafe(no_mangle))]
pub extern "C" fn ilogb(value: f64) -> i32 {
    if value == 0.0 || !value.is_finite() {
        invalid();
    }
    libm::ilogb(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn normal_and_subnormal_exponents_and_domain_errors() {
        #[cfg(target_arch = "aarch64")]
        let saved_flags: u64;
        #[cfg(target_arch = "aarch64")]
        unsafe {
            core::arch::asm!("mrs {saved}, fpsr", "msr fpsr, xzr", saved = out(reg) saved_flags, options(nostack));
        }
        unsafe {
            crate::__errno_location().write(78);
        }
        assert_eq!(ilogbf(1.0), 0);
        assert_eq!(ilogbf(-8.0), 3);
        assert_eq!(ilogbf(f32::MIN_POSITIVE), -126);
        assert_eq!(ilogbf(f32::from_bits(1)), -149);
        assert_eq!(ilogbf(f32::MAX), 127);
        assert_eq!(ilogb(f64::MIN_POSITIVE), -1022);
        assert_eq!(ilogb(f64::from_bits(1)), -1074);
        assert_eq!(ilogb(f64::MAX), 1023);
        assert_eq!(unsafe { crate::__errno_location().read() }, 78);
        assert_eq!(ilogbf(0.0), i32::MIN);
        assert_eq!(ilogb(f64::NAN), i32::MIN);
        assert_eq!(ilogb(f64::INFINITY), i32::MAX);
        assert_eq!(unsafe { crate::__errno_location().read() }, EDOM);
        #[cfg(target_arch = "aarch64")]
        unsafe {
            let flags: u64;
            core::arch::asm!("mrs {flags}, fpsr", flags = out(reg) flags, options(nostack));
            assert_ne!(flags & 1, 0);
            core::arch::asm!("msr fpsr, {saved}", saved = in(reg) saved_flags, options(nostack));
        }
    }
}
