// Cross-language benchmark harness for NanoLog (C++).
//
// Self-measures call-site latency using the same hardware counter as
// every other candidate (RDTSC on amd64, CNTVCT_EL0 on arm64). Accepts
// --ns-per-tick from the pre-calibration step and writes per-configuration
// percentiles plus throughput as JSON to --output.
//
// NanoLog is Linux-only. On other platforms this harness compiles but
// exits with an error at startup.

#ifdef __linux__

// NanoLogCpp17.h uses std::array without including <array>; recent libstdc++
// (GCC 16) no longer pulls it in transitively, so include it first.
#include <array>

#include "NanoLogCpp17.h"

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

static inline uint64_t read_counter() {
#if defined(__x86_64__) || defined(_M_X64)
    unsigned int lo, hi;
    __asm__ volatile("rdtsc" : "=a"(lo), "=d"(hi));
    return (static_cast<uint64_t>(hi) << 32) | lo;
#elif defined(__aarch64__)
    uint64_t val;
    __asm__ volatile("mrs %0, cntvct_el0" : "=r"(val));
    return val;
#else
    // Fallback: CLOCK_MONOTONIC. Not zero-overhead but lets arm32 compile.
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return static_cast<uint64_t>(ts.tv_sec) * 1'000'000'000ULL + static_cast<uint64_t>(ts.tv_nsec);
#endif
}

static inline uint64_t read_cntfrq() {
#if defined(__aarch64__)
    uint64_t val;
    __asm__ volatile("mrs %0, cntfrq_el0" : "=r"(val));
    return val;
#else
    return 0;
#endif
}

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

// run_batch executes BATCH log calls.
// NanoLog uses printf-style format strings. call_index varies across the
// run so the compiler cannot constant-fold the log site.
static void run_batch(Workload wl, uint64_t call_index) {
    switch (wl) {
        case Workload::SingleInt:
            for (int i = 0; i < BATCH; i++) {
                uint64_t v = call_index * static_cast<uint64_t>(BATCH) + static_cast<uint64_t>(i);
                NANO_LOG(NOTICE, "x=%lu", v);
            }
            break;
        case Workload::Mixed:
            for (int i = 0; i < BATCH; i++) {
                NANO_LOG(NOTICE, "%lu %f %s",
                    static_cast<unsigned long>(42), 3.14159, "hello world");
            }
            break;
        case Workload::String:
            for (int i = 0; i < BATCH; i++) {
                NANO_LOG(NOTICE, "%s", "hello world");
            }
            break;
    }
}

// -- Percentiles --------------------------------------------------------

// percentile computes the nearest-rank percentile.
static double percentile(const std::vector<double>& sorted, double p) {
    double n = static_cast<double>(sorted.size());
    int rank = static_cast<int>(std::ceil(n * p)) - 1;
    if (rank < 0) rank = 0;
    if (rank >= static_cast<int>(sorted.size())) rank = static_cast<int>(sorted.size()) - 1;
    return sorted[static_cast<size_t>(rank)];
}

// -- Pacing -------------------------------------------------------------

static void random_pause(double ns_per_tick) {
    uint64_t seed = read_counter();
    uint64_t range = PACE_MAX_US - PACE_MIN_US;
    uint64_t r = (seed * 6364136223846793005ULL + 1) % (range + 1);
    uint64_t us = PACE_MIN_US + r;
    double ns = static_cast<double>(us) * 1000.0;
    uint64_t ticks = static_cast<uint64_t>(ns / ns_per_tick);
    uint64_t target = read_counter();
    while (read_counter() - target < ticks) {}
}

// -- Spin barrier -------------------------------------------------------

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

static ConfigResult measure_config(double ns_per_tick, Workload wl, int n_threads) {
    int samples_per_thread = SAMPLES / n_threads;

    std::vector<std::vector<double>> latencies(static_cast<size_t>(n_threads));
    for (int t = 0; t < n_threads; t++) {
        latencies[static_cast<size_t>(t)].reserve(static_cast<size_t>(samples_per_thread));
    }

    SpinBarrier barrier(n_threads);
    std::vector<std::thread> threads;
    threads.reserve(static_cast<size_t>(n_threads));

    auto wall_start = std::chrono::steady_clock::now();

    for (int t = 0; t < n_threads; t++) {
        threads.emplace_back([&, t, samples_per_thread]() {
            // Pre-allocate the per-thread staging buffer so the first
            // log call does not pay the allocation cost.
            NanoLog::preallocate();

            barrier.wait();

            auto& thread_lats = latencies[static_cast<size_t>(t)];

            for (int batch_i = 0; batch_i < samples_per_thread; batch_i++) {
                uint64_t call_index = static_cast<uint64_t>(t * samples_per_thread + batch_i);

                uint64_t t0 = read_counter();
                run_batch(wl, call_index);
                uint64_t t1 = read_counter();

                uint64_t ticks = t1 - t0;
                double ns = static_cast<double>(ticks) * ns_per_tick;
                double per_call_ns = ns / static_cast<double>(BATCH);
                thread_lats.push_back(per_call_ns);

                random_pause(ns_per_tick);
            }
        });
    }

    for (auto& th : threads) {
        th.join();
    }

    auto wall_end = std::chrono::steady_clock::now();
    double wall_duration_s = std::chrono::duration<double>(wall_end - wall_start).count();
    uint64_t throughput = static_cast<uint64_t>(std::round(
        static_cast<double>(TOTAL_MESSAGES) / wall_duration_s));

    size_t total = 0;
    for (const auto& sl : latencies) total += sl.size();
    std::vector<double> all;
    all.reserve(total);
    for (const auto& sl : latencies) all.insert(all.end(), sl.begin(), sl.end());
    std::sort(all.begin(), all.end());

    if (all.empty()) {
        std::fprintf(stderr, "nanolog harness: invariant violation -- no samples\n");
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

static double resolve_ns_per_tick(double flag_val) {
    uint64_t freq = read_cntfrq();

    if (freq > 0) {
        double nspt = 1'000'000'000.0 / static_cast<double>(freq);
        if (flag_val > 0.0 && std::abs(flag_val - nspt) > nspt * 0.01) {
            std::fprintf(stderr,
                "warn: --ns-per-tick=%.6f but CNTFRQ_EL0=%llu implies %.6f. "
                "Using CNTFRQ_EL0.\n",
                flag_val, static_cast<unsigned long long>(freq), nspt);
        }
        return nspt;
    }

    if (flag_val <= 0.0) {
        std::fprintf(stderr, "error: --ns-per-tick is required and must be positive\n");
        std::fprintf(stderr, "usage: nanolog_harness --ns-per-tick <float> --output <path.json>\n");
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
            std::fprintf(stderr, "usage: nanolog_harness --ns-per-tick <float> --output <path.json>\n");
            return 1;
        }
    }

    if (!output_path) {
        std::fprintf(stderr, "error: --output is required\n");
        std::fprintf(stderr, "usage: nanolog_harness --ns-per-tick <float> --output <path.json>\n");
        return 1;
    }

    double ns_per_tick = resolve_ns_per_tick(ns_per_tick_flag);

    // Discard all NanoLog output -- we measure the hot-path staging-buffer
    // write only, which is the same for every candidate. NanoLog opens its
    // output with O_DIRECT, which /dev/null rejects, so drain to a real temp
    // file on disk; the background compaction thread owns all I/O and never
    // touches the measured hot path. The caller removes this file afterward.
    NanoLog::setLogFile("/tmp/nanolog_bench.log");
    NanoLog::setLogLevel(NanoLog::LogLevels::NOTICE);

    // Pre-allocate the main thread's staging buffer.
    NanoLog::preallocate();

    std::vector<ConfigResult> results;

    for (int n_threads : THREAD_COUNTS) {
        for (Workload wl : ALL_WORKLOADS) {
            std::fprintf(stderr, "  %s threads=%d ...\n", workload_name(wl), n_threads);
            results.push_back(measure_config(ns_per_tick, wl, n_threads));
        }
    }

    const char* clock_name = "unknown";
#if defined(__x86_64__) || defined(_M_X64)
    clock_name = "rdtsc";
#elif defined(__aarch64__)
    clock_name = "cntvct_el0";
#endif

    Output out;
    out.candidate = "nanolog";
    out.os = "linux";
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

#else  // !__linux__

#include <cstdio>

int main() {
    std::fprintf(stderr, "error: NanoLog harness is Linux-only. Skipping.\n");
    // Exit 0 so run.sh can continue to the next candidate.
    return 0;
}

#endif
