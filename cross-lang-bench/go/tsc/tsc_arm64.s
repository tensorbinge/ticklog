#include "textflag.h"

// func ReadCounter() uint64
//
// Reads the ARM Generic Timer virtual counter on aarch64.
// CNTVCT_EL0 is available at EL0 on every ARMv8-A implementation.
// MRS reads the 64-bit counter into R0 with no side effects.
//
// NOTE: The effective frequency depends on the macOS SDK the binary
// was linked against.  SDK >= macOS 15 -> 1 GHz; older SDKs -> 24 MHz.
// Always read CNTFRQ_EL0 to get the actual frequency.
TEXT ·ReadCounter(SB),NOSPLIT,$0-8
	MRS	CNTVCT_EL0, R0
	MOVD	R0, ret+0(FP)
	RET

// func ReadCntfrq() uint64
//
// Reads CNTFRQ_EL0, the Counter Frequency register.
// Returns the number of ticks per second (Hz) for CNTVCT_EL0
// as visible from this binary's SDK context.
TEXT ·ReadCntfrq(SB),NOSPLIT,$0-8
	MRS	CNTFRQ_EL0, R0
	MOVD	R0, ret+0(FP)
	RET
