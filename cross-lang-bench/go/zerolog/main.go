// Cross-language benchmark harness for zerolog (Go).
//
// Self-measures call-site latency using the same hardware counter as
// every other candidate (RDTSC on amd64, CNTVCT_EL0 on arm64). Accepts
// --ns-per-tick from the pre-calibration step and writes per-configuration
// percentiles plus throughput as JSON to --output.
//
// On arm64 macOS the harness reads CNTFRQ_EL0 at startup to determine
// its own effective counter frequency.  This is necessary because macOS
// scales CNTVCT_EL0 based on the linked SDK version: binaries linked
// against the macOS 11 SDK see 24 MHz; binaries linked against macOS 15+
// see 1 GHz.  Self-calibration keeps the measurements correct regardless.
package main

import (
	"encoding/json"
	"flag"
	"fmt"
	"io"
	"math"
	"os"
	"path/filepath"
	"runtime"
	"sort"
	"strings"
	"sync"
	"time"

	"github.com/rs/zerolog"

	"ticklog-cross-lang-bench/tsc"
)

// Constants (must match the design doc)
const (
	// batch is the number of log calls between counter reads.
	batch = 1000

	// samples is the number of batch-average measurements per config.
	samples = 10_000

	// totalMessages is the total log messages per config: samples * batch.
	totalMessages = samples * batch

	// Pacing range between batches, microseconds.
	paceMinUs = 1000
	paceMaxUs = 3000
)

// threadCounts to benchmark.
var threadCounts = []int{1, 2, 4}

// Workload
type workload int

const (
	wlSingleInt workload = iota
	wlMixed
	wlString
)

func (w workload) name() string {
	switch w {
	case wlSingleInt:
		return "single_int"
	case wlMixed:
		return "mixed"
	case wlString:
		return "string"
	default:
		panic(fmt.Sprintf("unknown workload: %d", w))
	}
}

var allWorkloads = []workload{wlSingleInt, wlMixed, wlString}

// runBatch executes batch log calls against the provided logger.
// callIndex varies across the run so the compiler cannot constant-fold
// the log site.
func (w workload) runBatch(logger zerolog.Logger, callIndex uint64) {
	switch w {
	case wlSingleInt:
		for i := 0; i < batch; i++ {
			v := callIndex*uint64(batch) + uint64(i)
			logger.Info().Uint64("x", v).Msg("")
		}
	case wlMixed:
		for i := 0; i < batch; i++ {
			logger.Info().
				Uint64("a", 42).
				Float64("b", 3.14159).
				Str("c", "hello world").
				Msg("")
		}
	case wlString:
		for i := 0; i < batch; i++ {
			logger.Info().Str("s", "hello world").Msg("")
		}
	}
}

// Percentiles
// percentile computes the nearest-rank percentile.
// p in (0, 1], e.g. 0.50 for p50.
// sorted must be sorted ascending and non-empty.
func percentile(sorted []float64, p float64) float64 {
	n := float64(len(sorted))
	rank := int(math.Ceil(n*p)) - 1
	if rank < 0 {
		rank = 0
	}
	if rank >= len(sorted) {
		rank = len(sorted) - 1
	}
	return sorted[rank]
}

// Pacing
// randomPause busy-waits for a random duration in [1, 3] milliseconds.
//
// Uses a simple LCG seeded from the counter and the resolved ns_per_tick
// to convert microseconds to counter ticks accurately on any platform.
func randomPause(nsPerTick float64) {
	seed := tsc.ReadCounter()
	rangeUs := uint64(paceMaxUs - paceMinUs)
	r := (seed*6364136223846793005 + 1) % (rangeUs + 1)
	us := paceMinUs + r
	// Convert microseconds to counter ticks: divide by ns_per_tick.
	// At 1 tick/ns (1 GHz):   us * 1000 ticks.
	// At 24 MHz:              us * 24   ticks (approximately).
	ns := float64(us) * 1000.0
	ticks := uint64(ns / nsPerTick)
	target := tsc.ReadCounter()
	for tsc.ReadCounter()-target < ticks {
		// busy-wait — the counter call prevents loop elimination.
	}
}

// Measurement
// configResult is one row in the output JSON array.
type configResult struct {
	Workload   string  `json:"workload"`
	Threads    int     `json:"threads"`
	Throughput uint64  `json:"throughput"`
	P50        float64 `json:"p50"`
	P95        float64 `json:"p95"`
	P99        float64 `json:"p99"`
	P999       float64 `json:"p999"`
	Max        float64 `json:"max"`
}

// output is the top-level JSON schema shared by every harness.
type output struct {
	Candidate     string         `json:"candidate"`
	OS            string         `json:"os"`
	Arch          string         `json:"arch"`
	Clock         string         `json:"clock"`
	NsPerTick     float64        `json:"ns_per_tick"`
	BatchSize     int            `json:"batch_size"`
	TotalMessages uint64         `json:"total_messages"`
	NumSamples    int            `json:"samples"`
	PacingUs      [2]uint64      `json:"pacing_us"`
	Results       []configResult `json:"results"`
}

// measureConfig runs one (workload, threadCount) configuration and
// returns the measured percentiles and throughput.
func measureConfig(nsPerTick float64, wl workload, nThreads int) configResult {
	samplesPerThread := samples / nThreads

	var wg sync.WaitGroup
	latencies := make([][]float64, nThreads)
	for i := range latencies {
		latencies[i] = make([]float64, 0, samplesPerThread)
	}

	wallStart := time.Now()

	for t := 0; t < nThreads; t++ {
		wg.Add(1)
		go func(threadIdx int) {
			defer wg.Done()

			// Pin this goroutine to an OS thread so we
			// measure real thread-level parallelism, not
			// goroutine multiplexing.
			runtime.LockOSThread()
			defer runtime.UnlockOSThread()

			// Create a discarded logger — it does all the
			// hot-path formatting and event construction but
			// writes to /dev/null.  Equivalent to the ticklog
			// NullSink that discards in accept().
			logger := zerolog.New(io.Discard).Level(zerolog.InfoLevel)

			threadLats := latencies[threadIdx]
			for batchI := 0; batchI < samplesPerThread; batchI++ {
				callIndex := uint64(threadIdx*samplesPerThread + batchI)

				t0 := tsc.ReadCounter()
				wl.runBatch(logger, callIndex)
				t1 := tsc.ReadCounter()

				ticks := t1 - t0
				ns := float64(ticks) * nsPerTick
				perCallNs := ns / float64(batch)
				threadLats = append(threadLats, perCallNs)

				randomPause(nsPerTick)
			}
			latencies[threadIdx] = threadLats
		}(t)
	}
	wg.Wait()

	wallDurationS := time.Since(wallStart).Seconds()
	throughput := uint64(math.Round(float64(totalMessages) / wallDurationS))

	// Merge all per-thread slices into a single sorted vector.
	total := 0
	for _, sl := range latencies {
		total += len(sl)
	}
	all := make([]float64, 0, total)
	for _, sl := range latencies {
		all = append(all, sl...)
	}
	sort.Float64s(all)

	if len(all) == 0 {
		panic("invariant: at least one sample")
	}

	r2 := func(x float64) float64 { return math.Round(x*100) / 100 }

	return configResult{
		Workload:   wl.name(),
		Threads:    nThreads,
		Throughput: throughput,
		P50:        r2(percentile(all, 0.50)),
		P95:        r2(percentile(all, 0.95)),
		P99:        r2(percentile(all, 0.99)),
		P999:       r2(percentile(all, 0.999)),
		Max:        r2(all[len(all)-1]),
	}
}

// Self-calibration (arm64)
// resolveNsPerTick determines the correct ns_per_tick for this binary.
//
// On arm64 we read CNTFRQ_EL0 to get the effective counter frequency
// that macOS is presenting to this binary's SDK level.  This is the
// only way to be correct when the Go linker embeds a different SDK
// version than the C calibrate binary.
//
// On amd64 CNTFRQ_EL0 is not available; --ns-per-tick from calibrate.c
// is required and is the sole source of truth.
func resolveNsPerTick(flagVal float64) float64 {
	freq := tsc.ReadCntfrq()

	if freq > 0 {
		// arm64: compute from the hardware frequency register.
		nspt := 1_000_000_000.0 / float64(freq)
		if flagVal > 0 && math.Abs(flagVal-nspt) > nspt*0.01 {
			fmt.Fprintf(os.Stderr,
				"warn: --ns-per-tick=%.6f but CNTFRQ_EL0=%d implies %.6f. "+
					"Using CNTFRQ_EL0 (Go binary linked against a different SDK than calibrate.c).\n",
				flagVal, freq, nspt)
		}
		return nspt
	}

	// amd64 (or arm64 without CNTFRQ_EL0): require the flag.
	if flagVal <= 0 {
		fmt.Fprintln(os.Stderr, "error: --ns-per-tick is required and must be positive")
		fmt.Fprintf(os.Stderr, "usage: %s --ns-per-tick <float> --output <path.json>\n",
			filepath.Base(os.Args[0]))
		os.Exit(1)
	}
	return flagVal
}

// CLI
func main() {
	nsPerTickFlag := flag.Float64("ns-per-tick", 0, "nanoseconds per counter tick (from calibrate.c)")
	outputPath := flag.String("output", "", "path to write JSON results")
	flag.Parse()

	if *outputPath == "" {
		fmt.Fprintln(os.Stderr, "error: --output is required")
		fmt.Fprintf(os.Stderr, "usage: %s --ns-per-tick <float> --output <path.json>\n",
			filepath.Base(os.Args[0]))
		os.Exit(1)
	}

	nsPerTick := resolveNsPerTick(*nsPerTickFlag)

	// Match GOMAXPROCS to the maximum thread count we will test so
	// that LockOSThread goroutines land on distinct OS threads.
	runtime.GOMAXPROCS(4)

	var results []configResult

	for _, nThreads := range threadCounts {
		for _, wl := range allWorkloads {
			fmt.Fprintf(os.Stderr, "  %s threads=%d ...\n", wl.name(), nThreads)
			results = append(results, measureConfig(nsPerTick, wl, nThreads))
		}
	}

	clockName := "unknown"
	switch runtime.GOARCH {
	case "amd64":
		clockName = "rdtsc"
	case "arm64":
		clockName = "cntvct_el0"
	}

	goos := strings.ToLower(runtime.GOOS)

	out := output{
		Candidate:     "zerolog",
		OS:            goos,
		Arch:          runtime.GOARCH,
		Clock:         clockName,
		NsPerTick:     nsPerTick,
		BatchSize:     batch,
		TotalMessages: totalMessages,
		NumSamples:    samples,
		PacingUs:      [2]uint64{paceMinUs, paceMaxUs},
		Results:       results,
	}

	jsonBytes, err := json.MarshalIndent(out, "", "  ")
	if err != nil {
		fmt.Fprintf(os.Stderr, "error: JSON marshal: %v\n", err)
		os.Exit(1)
	}

	if err := os.WriteFile(*outputPath, jsonBytes, 0o644); err != nil {
		fmt.Fprintf(os.Stderr, "error: write output: %v\n", err)
		os.Exit(1)
	}

	fmt.Fprintf(os.Stderr, "done -> %s\n", *outputPath)
}
