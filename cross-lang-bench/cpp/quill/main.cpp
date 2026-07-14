// Cross-language benchmark harness for Quill (C++).
//
// Self-measures call-site latency using the same hardware counter as
// every other candidate (RDTSC on x86_64, CNTVCT_EL0 on arm64). Accepts
// --ns-per-tick from the pre-calibration step and writes per-configuration
// percentiles plus throughput as JSON to --output.
//
// On arm64 macOS reads CNTFRQ_EL0 at startup to determine its own
// effective counter frequency (macOS scales the counter based on the
// linked SDK version -- see Go harnesses for rationale).

#include "quill/Backend.h"
#include "quill/Frontend.h"
#include "quill/LogMacros.h"
#include "quill/Logger.h"
#include "quill/sinks/NullSink.h"

#include <algorithm>
#include <atomic>
#include <chrono>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <string>
#include <thread>
#include <vector>

// -- Constants (must match the design doc) ------------------------------

static constexpr int BATCH = 1000;
static constexpr int SAMPLES = 10'000;
static constexpr uint64_t TOTAL_MESSAGES = static_cast<uint64_t>(SAMPLES) * BATCH;
static constexpr uint64_t PACE_MIN_US = 1000;
static constexpr uint64_t PACE_MAX_US = 3000;
static constexpr int THREAD_COUNTS[] = {1, 2, 4};

// -- Platform counter ---------------------------------------------------

#if defined(__x86_64__) || defined(_M_X64)
static inline uint64_t read_counter() {
    unsigned int lo, hi;
    __asm__ volatile("rdtsc" : "=a"(lo), "=d"(hi));
    return (static_cast<uint64_t>(hi) << 32) | lo;
}
static inline uint64_t read_cntfrq() { return 0; }
#elif defined(__aarch64__)
static inline uint64_t read_counter() {
    uint64_t val;
    __asm__ volatile("mrs %0, cntvct_el0" : "=r"(val));
    return val;
}
static inline uint64_t read_cntfrq() {
    uint64_t val;
    __asm__ volatile("mrs %0, cntfrq_el0" : "=r"(val));
    return val;
}
#else
#  error "unsupported architecture -- expected x86_64 or aarch64"
#endif

// -- Workload -----------------------------------------------------------

enum class Workload { SingleInt, Mixed, String };

const char* workload_name(Workload w) {
    switch (w) {
        case Workload::SingleInt: return "single_int";
        case Workload::Mixed:     return "mixed";
        case Workload::String:    return "string";
    }
    return "unknown";
}

Workload ALL_WORKLOADS[] = {Workload::SingleInt, Workload::Mixed, Workload::String};

// run_batch executes BATCH log calls against the provided logger.
// call_index varies across the run so the compiler cannot constant-fold
// the log site.
static void run_batch(quill::Logger* logger, Workload wl, uint64_t call_index) {
    switch (wl) {
        case Workload::SingleInt:
            for (int i = 0; i < BATCH; i++) {
                uint64_t v = call_index * static_cast<uint64_t>(BATCH) + static_cast<uint64_t>(i);
                LOG_INFO(logger, "x={}", v);
            }
            break;
        case Workload::Mixed:
            for (int i = 0; i < BATCH; i++) {
                LOG_INFO(logger, "{} {} {}", static_cast<uint64_t>(42), 3.14159, "hello world");
            }
            break;
        case Workload::String:
            for (int i = 0; i < BATCH; i++) {
                LOG_INFO(logger, "{}", "hello world");
            }
            break;
    }
}

// -- Percentiles --------------------------------------------------------

// percentile computes the nearest-rank percentile.
// p in (0, 1], e.g. 0.50 for p50.
// sorted must be sorted ascending and non-empty.
static double percentile(const std::vector<double>& sorted, double p) {
    double n = static_cast<double>(sorted.size());
    int rank = static_cast<int>(std::ceil(n * p)) - 1;
    if (rank < 0) rank = 0;
    if (rank >= static_cast<int>(sorted.size())) rank = static_cast<int>(sorted.size()) - 1;
    return sorted[static_cast<size_t>(rank)];
}

// -- Pacing -------------------------------------------------------------

// random_pause busy-waits for a random duration in [1, 3] milliseconds.
// Uses a simple LCG seeded from the counter and the resolved ns_per_tick
// to convert microseconds to counter ticks.
static void random_pause(double ns_per_tick) {
    uint64_t seed = read_counter();
    uint64_t range = PACE_MAX_US - PACE_MIN_US;
    uint64_t r = (seed * 6364136223846793005ULL + 1) % (range + 1);
    uint64_t us = PACE_MIN_US + r;
    double ns = static_cast<double>(us) * 1000.0;
    uint64_t ticks = static_cast<uint64_t>(ns / ns_per_tick);
    uint64_t target = read_counter();
    while (read_counter() - target < ticks) {
        // busy-wait -- the counter call prevents loop elimination.
    }
}

// -- Spin barrier (C++17 compatible) ------------------------------------

struct SpinBarrier {
    std::atomic<int> count;
    explicit SpinBarrier(int n) : count(n) {}
    void wait() {
        count.fetch_sub(1, std::memory_order_acq_rel);
        while (count.load(std::memory_order_acquire) > 0) {}
    }
};

// -- Measurement --------------------------------------------------------

struct ConfigResult {
    std::string workload;
    int threads;
    uint64_t throughput;
    double p50, p95, p99, p999, max;
};

struct Output {
    std::string candidate;
    std::string os;
    std::string arch;
    std::string clock;
    double ns_per_tick;
    int batch_size;
    uint64_t total_messages;
    int samples;
    uint64_t pacing_us[2];
    std::vector<ConfigResult> results;
};

// measure_config runs one (workload, thread_count) configuration and
// returns the measured percentiles and throughput.
static ConfigResult measure_config(double ns_per_tick, Workload wl, int n_threads) {
    int samples_per_thread = SAMPLES / n_threads;

    // Per-thread latency storage.
    std::vector<std::vector<double>> latencies(static_cast<size_t>(n_threads));
    for (int t = 0; t < n_threads; t++) {
        latencies[static_cast<size_t>(t)].reserve(static_cast<size_t>(samples_per_thread));
    }

    // Two barriers: one after warmup, one to start measurement.
    // The main thread participates so it can start the wall clock
    // between the two barriers, excluding warmup from throughput.
    SpinBarrier warmup_barrier(n_threads + 1);
    SpinBarrier start_barrier(n_threads + 1);

    std::vector<std::thread> threads;
    threads.reserve(static_cast<size_t>(n_threads));

    for (int t = 0; t < n_threads; t++) {
        threads.emplace_back([&, t, samples_per_thread]() {
            // Pre-allocate thread-local SPSC queue.
            quill::Frontend::preallocate();

            // Create a logger backed by NullSink.
            auto null_sink = quill::Frontend::create_or_get_sink<quill::NullSink>("null");
            quill::Logger* logger = quill::Frontend::create_or_get_logger(
                "root", std::move(null_sink));

            // Warmup: drive enough log calls to let the SPSC queue resize
            // to its steady-state capacity.
            for (int w = 0; w < 2000; w++) {
                LOG_INFO(logger, "warmup {}", w);
            }

            // Signal main thread and wait for measurement start.
            warmup_barrier.wait();
            start_barrier.wait();

            auto& thread_lats = latencies[static_cast<size_t>(t)];

            for (int batch_i = 0; batch_i < samples_per_thread; batch_i++) {
                uint64_t call_index = static_cast<uint64_t>(t * samples_per_thread + batch_i);

                uint64_t t0 = read_counter();
                run_batch(logger, wl, call_index);
                uint64_t t1 = read_counter();

                uint64_t ticks = t1 - t0;
                double ns = static_cast<double>(ticks) * ns_per_tick;
                double per_call_ns = ns / static_cast<double>(BATCH);
                thread_lats.push_back(per_call_ns);

                random_pause(ns_per_tick);
            }
        });
    }

    // Wait until all threads have finished warmup, then start the clock
    // and release them together.
    warmup_barrier.wait();
    auto wall_start = std::chrono::steady_clock::now();
    start_barrier.wait();

    for (auto& th : threads) {
        th.join();
    }

    auto wall_end = std::chrono::steady_clock::now();
    double wall_duration_s = std::chrono::duration<double>(wall_end - wall_start).count();
    uint64_t throughput = static_cast<uint64_t>(std::round(
        static_cast<double>(TOTAL_MESSAGES) / wall_duration_s));

    // Merge all per-thread slices into a single sorted vector.
    size_t total = 0;
    for (const auto& sl : latencies) total += sl.size();
    std::vector<double> all;
    all.reserve(total);
    for (const auto& sl : latencies) all.insert(all.end(), sl.begin(), sl.end());
    std::sort(all.begin(), all.end());

    if (all.empty()) {
        std::fprintf(stderr, "quill harness: invariant violation -- no samples\n");
        std::exit(1);
    }

    auto r2 = [](double x) { return std::round(x * 100.0) / 100.0; };

    return ConfigResult{
        workload_name(wl),
        n_threads,
        throughput,
        r2(percentile(all, 0.50)),
        r2(percentile(all, 0.95)),
        r2(percentile(all, 0.99)),
        r2(percentile(all, 0.999)),
        r2(all.back()),
    };
}

// -- Self-calibration (arm64) -------------------------------------------

// resolve_ns_per_tick determines the correct ns_per_tick for this binary.
//
// On arm64 we read CNTFRQ_EL0 to get the effective counter frequency that
// macOS is presenting to this binary's SDK level. This is the only way to
// be correct when a binary is linked against a different SDK than the C
// calibrate binary.
//
// On x86_64 CNTFRQ_EL0 is not available; --ns-per-tick from calibrate.c
// is required.
static double resolve_ns_per_tick(double flag_val) {
    uint64_t freq = read_cntfrq();

    if (freq > 0) {
        // arm64: compute from the hardware frequency register.
        double nspt = 1'000'000'000.0 / static_cast<double>(freq);
        if (flag_val > 0.0 && std::abs(flag_val - nspt) > nspt * 0.01) {
            std::fprintf(stderr,
                "warn: --ns-per-tick=%.6f but CNTFRQ_EL0=%llu implies %.6f. "
                "Using CNTFRQ_EL0 (binary linked against a different SDK than calibrate.c).\n",
                flag_val, static_cast<unsigned long long>(freq), nspt);
        }
        return nspt;
    }

    // x86_64 (or arm64 without CNTFRQ_EL0): require the flag.
    if (flag_val <= 0.0) {
        std::fprintf(stderr, "error: --ns-per-tick is required and must be positive\n");
        std::fprintf(stderr, "usage: quill_harness --ns-per-tick <float> --output <path.json>\n");
        std::exit(1);
    }
    return flag_val;
}

// -- JSON output --------------------------------------------------------

static void write_output(const Output& out, const char* path) {
    std::FILE* f = std::fopen(path, "w");
    if (!f) {
        std::fprintf(stderr, "error: cannot open %s for writing\n", path);
        std::exit(1);
    }

    std::fprintf(f, "{\n");
    std::fprintf(f, "  \"candidate\": \"%s\",\n", out.candidate.c_str());
    std::fprintf(f, "  \"os\": \"%s\",\n", out.os.c_str());
    std::fprintf(f, "  \"arch\": \"%s\",\n", out.arch.c_str());
    std::fprintf(f, "  \"clock\": \"%s\",\n", out.clock.c_str());
    std::fprintf(f, "  \"ns_per_tick\": %.6f,\n", out.ns_per_tick);
    std::fprintf(f, "  \"batch_size\": %d,\n", out.batch_size);
    std::fprintf(f, "  \"total_messages\": %llu,\n", static_cast<unsigned long long>(out.total_messages));
    std::fprintf(f, "  \"samples\": %d,\n", out.samples);
    std::fprintf(f, "  \"pacing_us\": [%llu, %llu],\n",
        static_cast<unsigned long long>(out.pacing_us[0]),
        static_cast<unsigned long long>(out.pacing_us[1]));
    std::fprintf(f, "  \"results\": [\n");

    for (size_t i = 0; i < out.results.size(); i++) {
        const auto& r = out.results[i];
        const char* comma = (i + 1 < out.results.size()) ? "," : "";
        std::fprintf(f, "    {\n");
        std::fprintf(f, "      \"workload\": \"%s\",\n", r.workload.c_str());
        std::fprintf(f, "      \"threads\": %d,\n", r.threads);
        std::fprintf(f, "      \"throughput\": %llu,\n", static_cast<unsigned long long>(r.throughput));
        std::fprintf(f, "      \"p50\": %.2f,\n", r.p50);
        std::fprintf(f, "      \"p95\": %.2f,\n", r.p95);
        std::fprintf(f, "      \"p99\": %.2f,\n", r.p99);
        std::fprintf(f, "      \"p999\": %.2f,\n", r.p999);
        std::fprintf(f, "      \"max\": %.2f\n", r.max);
        std::fprintf(f, "    }%s\n", comma);
    }
    std::fprintf(f, "  ]\n");
    std::fprintf(f, "}\n");

    std::fclose(f);
}

// -- main ---------------------------------------------------------------

int main(int argc, char* argv[]) {
    double ns_per_tick_flag = 0.0;
    const char* output_path = nullptr;

    for (int i = 1; i < argc; i++) {
        if (std::strcmp(argv[i], "--ns-per-tick") == 0 && i + 1 < argc) {
            ns_per_tick_flag = std::strtod(argv[++i], nullptr);
        } else if (std::strcmp(argv[i], "--output") == 0 && i + 1 < argc) {
            output_path = argv[++i];
        } else {
            std::fprintf(stderr, "error: unknown flag '%s'\n", argv[i]);
            std::fprintf(stderr, "usage: quill_harness --ns-per-tick <float> --output <path.json>\n");
            return 1;
        }
    }

    if (!output_path) {
        std::fprintf(stderr, "error: --output is required\n");
        std::fprintf(stderr, "usage: quill_harness --ns-per-tick <float> --output <path.json>\n");
        return 1;
    }

    double ns_per_tick = resolve_ns_per_tick(ns_per_tick_flag);

    // Start the Quill backend thread. It processes the SPSC queues and
    // hands log statements to sinks (our NullSink discards them).
    quill::Backend::start();

    std::vector<ConfigResult> results;

    for (int n_threads : THREAD_COUNTS) {
        for (Workload wl : ALL_WORKLOADS) {
            std::fprintf(stderr, "  %s threads=%d ...\n", workload_name(wl), n_threads);
            results.push_back(measure_config(ns_per_tick, wl, n_threads));
        }
    }

    // Determine clock name.
    const char* clock_name = "unknown";
#if defined(__x86_64__) || defined(_M_X64)
    clock_name = "rdtsc";
#elif defined(__aarch64__)
    clock_name = "cntvct_el0";
#endif

    // Determine OS.
    const char* os_name = "unknown";
#if defined(__linux__)
    os_name = "linux";
#elif defined(__APPLE__)
    os_name = "macos";
#elif defined(_WIN32)
    os_name = "windows";
#endif

    Output out;
    out.candidate = "quill";
    out.os = os_name;
    out.arch =
#if defined(__x86_64__) || defined(_M_X64)
        "x86_64";
#elif defined(__aarch64__)
        "aarch64";
#else
        "unknown";
#endif
    out.clock = clock_name;
    out.ns_per_tick = ns_per_tick;
    out.batch_size = BATCH;
    out.total_messages = TOTAL_MESSAGES;
    out.samples = SAMPLES;
    out.pacing_us[0] = PACE_MIN_US;
    out.pacing_us[1] = PACE_MAX_US;
    out.results = std::move(results);

    write_output(out, output_path);

    std::fprintf(stderr, "done -> %s\n", output_path);
    return 0;
}
