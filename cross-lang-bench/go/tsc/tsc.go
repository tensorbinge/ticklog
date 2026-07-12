// Package tsc provides access to the platform hardware counter.
//
// Every harness in the cross-language benchmark suite reads the same
// counter (RDTSC on amd64, CNTVCT_EL0 on arm64) so that ns_per_tick
// from calibrate.c converts raw ticks to nanoseconds identically
// across Rust, C++, and Go.
package tsc

// ReadCounter reads the platform-specific monotonic hardware counter.
//
// On amd64: invariant Time Stamp Counter via RDTSC.
// On arm64: ARM Generic Timer virtual counter via CNTVCT_EL0.
// Other architectures: linker fails with missing function body.
func ReadCounter() uint64

// ReadCntfrq reads the Counter Frequency register (CNTFRQ_EL0).
//
// Returns the effective counter frequency in Hz on arm64.
// Returns 0 on amd64 (no architectural frequency register for TSC).
func ReadCntfrq() uint64
