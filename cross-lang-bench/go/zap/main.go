// Cross-language benchmark harness for zap (Go).
//
// Self-measures call-site latency using the same hardware counter as
// every other candidate (RDTSC on amd64, CNTVCT_EL0 on arm64).
// On arm64 macOS reads CNTFRQ_EL0 at startup to determine its own
// effective counter frequency (Go linkers embed a different SDK than
// the C calibrate binary; see the zerolog harness for rationale).
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

	"go.uber.org/zap"
	"go.uber.org/zap/zapcore"

	"ticklog-cross-lang-bench/tsc"
)

// Constants (must match the design doc)
const (
	batch         = 1000
	samples       = 10_000
	totalMessages = samples * batch
)

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
func (w workload) runBatch(logger *zap.Logger, callIndex uint64) {
	switch w {
	case wlSingleInt:
		for i := 0; i < batch; i++ {
			v := callIndex*uint64(batch) + uint64(i)
			logger.Info("", zap.Uint64("x", v))
		}
	case wlMixed:
		for i := 0; i < batch; i++ {
			logger.Info("",
				zap.Uint64("a", 42),
				zap.Float64("b", 3.14159),
				zap.String("c", "hello world"),
			)
		}
	case wlString:
		for i := 0; i < batch; i++ {
			logger.Info("", zap.String("s", "hello world"))
		}
	}
}

// Percentiles
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

// Measurement
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

type output struct {
	Candidate     string         `json:"candidate"`
	OS            string         `json:"os"`
	Arch          string         `json:"arch"`
	Clock         string         `json:"clock"`
	NsPerTick     float64        `json:"ns_per_tick"`
	BatchSize     int            `json:"batch_size"`
	TotalMessages uint64         `json:"total_messages"`
	NumSamples    int            `json:"samples"`
	Results       []configResult `json:"results"`
}

// newDiscardLogger returns a zap.Logger that does full JSON encoding
// but discards the output, equivalent to the ticklog NullSink.
func newDiscardLogger() *zap.Logger {
	enc := zapcore.NewJSONEncoder(zap.NewProductionEncoderConfig())
	core := zapcore.NewCore(enc, zapcore.AddSync(io.Discard), zapcore.InfoLevel)
	return zap.New(core)
}

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
			runtime.LockOSThread()
			defer runtime.UnlockOSThread()

			logger := newDiscardLogger()
			defer logger.Sync()

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

			}
			latencies[threadIdx] = threadLats
		}(t)
	}
	wg.Wait()

	wallDurationS := time.Since(wallStart).Seconds()
	throughput := uint64(math.Round(float64(totalMessages) / wallDurationS))

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
func resolveNsPerTick(flagVal float64) float64 {
	freq := tsc.ReadCntfrq()

	if freq > 0 {
		nspt := 1_000_000_000.0 / float64(freq)
		if flagVal > 0 && math.Abs(flagVal-nspt) > nspt*0.01 {
			fmt.Fprintf(os.Stderr,
				"warn: --ns-per-tick=%.6f but CNTFRQ_EL0=%d implies %.6f. "+
					"Using CNTFRQ_EL0 (Go binary linked against a different SDK than calibrate.c).\n",
				flagVal, freq, nspt)
		}
		return nspt
	}

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
		Candidate:     "zap",
		OS:            goos,
		Arch:          runtime.GOARCH,
		Clock:         clockName,
		NsPerTick:     nsPerTick,
		BatchSize:     batch,
		TotalMessages: totalMessages,
		NumSamples:    samples,
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
