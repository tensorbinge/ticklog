//! Platform-specific thread-CPU affinity.
//!
//! On Linux the caller can pin to a set of logical cores via `sched_setaffinity`.
//! On macOS the Mach `thread_affinity_policy` API accepts an opaque affinity tag;
//! threads that share a tag are co-scheduled onto the same core cluster by the
//! scheduler. On other platforms these functions are no-ops.

/// Pin the calling thread to the given set of logical CPUs.
///
/// # Platform behaviour
///
/// | OS      | Semantics                                                    |
/// |---------|--------------------------------------------------------------|
/// | Linux   | Sets the CPU affinity mask to the union of the given cores   |
/// |         | via `sched_setaffinity`. Pass multiple cores to restrict the |
/// |         | thread to a cluster (e.g. all P-cores).                      |
/// | macOS   | Uses the first core as an opaque affinity tag; threads with  |
/// |         | the same tag are co-scheduled by the Mach scheduler. Extra   |
/// |         | cores are ignored (macOS has no multi-core mask).            |
/// | Other   | No-op.                                                       |
///
/// An empty slice is a no-op on all platforms.
pub fn pin_thread(cores: &[usize]) {
    if cores.is_empty() {
        return;
    }
    #[cfg(target_os = "linux")]
    // SAFETY: `pin_linux` has no caller-visible precondition; it builds its
    // own cpu_set_t and issues a single `sched_setaffinity` syscall for the
    // calling thread (pid 0).
    unsafe {
        pin_linux(cores);
    }
    #[cfg(target_os = "macos")]
    // SAFETY: the `cores.is_empty()` early return above guarantees `cores[0]` is
    // in bounds; `pin_macos` has no other precondition (it only issues Mach
    // thread-policy calls over a stack-allocated struct).
    unsafe {
        pin_macos(cores[0]);
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let _ = cores;
}

// ---------------------------------------------------------------------------
// Linux: sched_setaffinity
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
mod linux_ffi {
    use std::ffi::c_int;

    // glibc cpu_set_t is 128 bytes (1024 bits); musl is similar. We allocate
    // one on the stack zeroed and set the requested bits.
    const CPU_SETSIZE: usize = 128;

    unsafe extern "C" {
        fn sched_setaffinity(
            pid: c_int, // 0 = calling thread
            cpusetsize: usize,
            mask: *const u8,
        ) -> c_int;
    }

    pub unsafe fn pin(cores: &[usize]) -> std::io::Result<()> {
        let mut set = [0u8; CPU_SETSIZE];
        for &core in cores {
            let byte = core / 8;
            let bit = core % 8;
            if byte < CPU_SETSIZE {
                set[byte] |= 1u8 << bit;
            }
        }
        // 0 pid = calling thread
        // SAFETY: FFI call with stack-allocated, correctly-sized cpu_set_t, pid=0
        // for the calling thread.
        let rc = unsafe { sched_setaffinity(0, CPU_SETSIZE, set.as_ptr()) };
        if rc == -1 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
unsafe fn pin_linux(cores: &[usize]) {
    // SAFETY: FFI call with a stack-allocated, correctly-sized cpu_set_t.
    if let Err(e) = unsafe { linux_ffi::pin(cores) } {
        eprintln!("ticklog: failed to pin thread to cores {cores:?}: {e}");
    }
}

// ---------------------------------------------------------------------------
// macOS: Mach thread_affinity_policy
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
mod macos_ffi {
    use std::ffi::{c_int, c_uint};

    type Natural = c_uint; // unsigned int
    type Integer = c_int; // int (signed)
    type MachPort = Natural;
    type KernReturn = c_int;
    type PolicyFlavor = Natural;
    type PolicyCount = Natural;

    const THREAD_AFFINITY_POLICY: PolicyFlavor = 4;
    const THREAD_AFFINITY_POLICY_COUNT: PolicyCount = 1;

    #[repr(C)]
    struct ThreadAffinityPolicy {
        affinity_tag: Integer,
    }

    unsafe extern "C" {
        /// The calling task's port name (the `mach_task_self()` macro in C).
        static mach_task_self_: MachPort;

        /// Returns the calling thread's Mach port, adding a `+1` send-right
        /// user-ref that the caller must release with `mach_port_deallocate`.
        fn mach_thread_self() -> MachPort;

        /// Set a scheduling policy on a thread.
        fn thread_policy_set(
            thread: MachPort,
            flavor: PolicyFlavor,
            policy_info: *const ThreadAffinityPolicy,
            count: PolicyCount,
        ) -> KernReturn;

        /// Release one user-ref on a port name held by a task.
        fn mach_port_deallocate(task: MachPort, name: MachPort) -> KernReturn;
    }

    /// `KERN_SUCCESS`.
    const KERN_SUCCESS: KernReturn = 0;

    pub unsafe fn pin(tag: usize) {
        let policy = ThreadAffinityPolicy {
            affinity_tag: tag as Integer,
        };
        // SAFETY: all four FFI calls are to Mach kernel routines in libSystem,
        // which is always linked on macOS. The policy struct is stack-allocated
        // and correctly sized; `mach_task_self_` is the task port name.
        let thread = unsafe { mach_thread_self() };
        let kr = unsafe {
            thread_policy_set(
                thread,
                THREAD_AFFINITY_POLICY,
                &policy as *const ThreadAffinityPolicy,
                THREAD_AFFINITY_POLICY_COUNT,
            )
        };
        if kr != KERN_SUCCESS {
            eprintln!("ticklog: thread_policy_set failed to pin thread (kern_return {kr})");
        }
        // Release the +1 send-right that `mach_thread_self()` granted; otherwise
        // every call leaks one user-ref on this thread's control port.
        // SAFETY: `mach_port_deallocate` is a libSystem Mach routine (always
        // linked on macOS); `mach_task_self_` is the calling task's port name and
        // `thread` is the live port returned by `mach_thread_self()` above.
        unsafe { mach_port_deallocate(mach_task_self_, thread) };
    }
}

#[cfg(target_os = "macos")]
unsafe fn pin_macos(tag: usize) {
    // SAFETY: `macos_ffi::pin` has no caller-visible precondition; it builds
    // its own `thread_affinity_policy` struct and calls only Mach routines
    // (`mach_thread_self`, `thread_policy_set`, `mach_port_deallocate`) that are
    // always linked in libSystem on macOS, balancing the port ref-count it takes.
    unsafe { macos_ffi::pin(tag) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_slice_is_noop() {
        // Must not panic on any platform.
        pin_thread(&[]);
    }

    #[test]
    #[cfg_attr(miri, ignore = "FFI: mach_thread_self / sched_setaffinity not available under Miri")]
    fn single_core_does_not_panic() {
        pin_thread(&[0]);
    }

    #[test]
    #[cfg_attr(miri, ignore = "FFI: mach_thread_self / sched_setaffinity not available under Miri")]
    fn multiple_cores_does_not_panic() {
        pin_thread(&[0, 1, 2, 3]);
    }

    #[test]
    #[cfg_attr(miri, ignore = "FFI: mach_thread_self / sched_setaffinity not available under Miri")]
    fn successive_calls_does_not_panic() {
        pin_thread(&[0]);
        pin_thread(&[1, 2]);
        pin_thread(&[]);
    }

    #[test]
    #[cfg_attr(miri, ignore = "FFI: mach_thread_self / sched_setaffinity not available under Miri")]
    fn large_core_number_does_not_panic() {
        pin_thread(&[usize::MAX]);
    }

    // macOS `mach_thread_self()` grants a +1 send-right user-ref on the thread's
    // control port that must be released with `mach_port_deallocate`. This
    // asserts `pin_thread` is leak-neutral: the send-right ref count on this
    // thread's port is unchanged across many calls.
    #[cfg(target_os = "macos")]
    #[test]
    #[cfg_attr(miri, ignore = "FFI: Mach port routines not available under Miri")]
    fn pin_does_not_leak_thread_port_refs() {
        use std::ffi::{c_int, c_uint};

        unsafe extern "C" {
            static mach_task_self_: c_uint;
            fn mach_thread_self() -> c_uint;
            fn mach_port_deallocate(task: c_uint, name: c_uint) -> c_int;
            fn mach_port_get_refs(
                task: c_uint,
                name: c_uint,
                right: c_uint,
                refs: *mut c_uint,
            ) -> c_int;
        }
        const MACH_PORT_RIGHT_SEND: c_uint = 0;

        // Send-right user-ref count on this thread's control port right now. The
        // probe takes a +1 ref via `mach_thread_self` and releases it, so it is
        // itself leak-neutral and safe to call repeatedly.
        fn send_refs() -> u32 {
            // SAFETY: Mach routines in libSystem; `mach_task_self_` is the task
            // port name; the ref out-param is a valid stack slot.
            unsafe {
                let thread = mach_thread_self();
                let mut refs: c_uint = 0;
                let kr =
                    mach_port_get_refs(mach_task_self_, thread, MACH_PORT_RIGHT_SEND, &mut refs);
                assert_eq!(kr, 0, "mach_port_get_refs failed: {kr}");
                mach_port_deallocate(mach_task_self_, thread);
                refs
            }
        }

        let before = send_refs();
        for _ in 0..16 {
            pin_thread(&[0]);
        }
        let after = send_refs();
        assert_eq!(
            after, before,
            "pin_thread leaked {} thread-port send-right refs over 16 calls",
            after as i64 - before as i64
        );
    }

    // A mask naming only out-of-range cores builds an all-zero cpu_set_t, which
    // `sched_setaffinity` rejects with EINVAL. That failure must surface as an
    // error rather than being silently swallowed.
    #[cfg(target_os = "linux")]
    #[test]
    #[cfg_attr(miri, ignore = "FFI: sched_setaffinity not available under Miri")]
    fn invalid_mask_reports_error() {
        // usize::MAX maps to no representable bit -> all-zero mask -> EINVAL.
        // SAFETY: FFI into sched_setaffinity with a valid stack-allocated mask.
        let result = unsafe { linux_ffi::pin(&[usize::MAX]) };
        assert!(
            result.is_err(),
            "an all-zero affinity mask must report an error, got {result:?}"
        );
    }
}
