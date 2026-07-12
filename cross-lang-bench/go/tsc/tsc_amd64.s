#include "textflag.h"

// func ReadCounter() uint64
//
// Reads the invariant Time Stamp Counter on x86_64.
// RDTSC produces a 64-bit value in EDX:EAX; we combine the halves
// and return the result.
TEXT ·ReadCounter(SB),NOSPLIT,$0-8
	RDTSC
	SHLQ	$32, DX
	ORQ	DX, AX
	MOVQ	AX, ret+0(FP)
	RET

// func ReadCntfrq() uint64
//
// x86_64 has no architectural frequency register for the TSC.
// Returns 0 to signal "not available" so the harness falls back
// to --ns-per-tick from calibrate.c.
TEXT ·ReadCntfrq(SB),NOSPLIT,$0-8
	MOVQ	$0, ret+0(FP)
	RET
