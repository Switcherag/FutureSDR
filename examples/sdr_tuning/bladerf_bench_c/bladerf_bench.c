/*
 * bladerf_bench.c — Native libbladeRF reconfiguration latency benchmark
 *
 * Measures the raw libbladeRF API call latency for set_frequency,
 * set_sample_rate, and set_gain — bypassing SoapySDR, seify, and FutureSDR.
 *
 * Compare results with sdr-tuning-bench (Rust/SoapySDR) to isolate
 * per-layer overhead in the full stack.
 *
 * Build:
 *   make
 * or:
 *   gcc -O2 -o bladerf_bench bladerf_bench.c -lbladeRF -I/usr/local/include -L/usr/local/lib64
 *
 * Run:
 *   ./bladerf_bench [iterations]
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>
#include <time.h>
#include <libbladeRF.h>

#define DEFAULT_ITERATIONS 20
#define FREQ_A  2437000000ULL   /* 2.437 GHz */
#define FREQ_B  2480000000ULL   /* 2.480 GHz (+43 MHz) */
#define RATE_A  4000000         /* 4 MSPS */
#define RATE_B  20000000        /* 20 MSPS */
#define GAIN_A  40              /* dB */
#define GAIN_B  20              /* dB */

typedef struct {
    double *times_ms;
    int count;
    const char *name;
} bench_result_t;

static double now_ms(void)
{
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ts.tv_sec * 1000.0 + ts.tv_nsec / 1e6;
}

static double avg(const double *v, int n)
{
    double s = 0;
    for (int i = 0; i < n; i++) s += v[i];
    return s / n;
}

static double minv(const double *v, int n)
{
    double m = v[0];
    for (int i = 1; i < n; i++) if (v[i] < m) m = v[i];
    return m;
}

static double maxv(const double *v, int n)
{
    double m = v[0];
    for (int i = 1; i < n; i++) if (v[i] > m) m = v[i];
    return m;
}

static double stddev(const double *v, int n)
{
    double a = avg(v, n);
    double s = 0;
    for (int i = 0; i < n; i++) s += (v[i] - a) * (v[i] - a);
    return sqrt(s / n);
}

#define CHECK(call) do {                                       \
    int _s = (call);                                           \
    if (_s != 0) {                                             \
        fprintf(stderr, "ERROR: %s → %s\n", #call,            \
                bladerf_strerror(_s));                          \
        goto cleanup;                                          \
    }                                                          \
} while (0)

int main(int argc, char **argv)
{
    int n = (argc > 1) ? atoi(argv[1]) : DEFAULT_ITERATIONS;
    if (n < 1) n = DEFAULT_ITERATIONS;

    struct bladerf *dev = NULL;
    int status;
    FILE *csv = NULL;

    /* Results for both TX and RX */
    const char *directions[] = {"tx", "rx"};
    bladerf_channel channels[] = {BLADERF_CHANNEL_TX(0), BLADERF_CHANNEL_RX(0)};
    int num_dirs = 2;

    /* 7 tests per direction */
    const int num_tests = 7;
    const char *test_names[] = {
        "freq", "sample_rate", "gain",
        "freq+rate", "freq+rate+gain",
        "freq_noop", "freq_1mhz"
    };

    /* Allocate result storage */
    int total_results = num_dirs * num_tests;
    bench_result_t *results = calloc(total_results, sizeof(bench_result_t));
    for (int i = 0; i < total_results; i++) {
        results[i].times_ms = calloc(n, sizeof(double));
        results[i].count = n;
    }

    /* ── Open device ── */
    printf("=== bladeRF Native Benchmark (libbladeRF direct) ===\n");
    printf("Iterations: %d\n", n);
    printf("Freq:       %.3f <-> %.3f MHz\n", FREQ_A / 1e6, FREQ_B / 1e6);
    printf("Rate:       %.1f <-> %.1f MSPS\n", RATE_A / 1e6, RATE_B / 1e6);
    printf("Gain:       %d <-> %d dB\n", GAIN_A, GAIN_B);
    printf("\n");

    double t0 = now_ms();
    status = bladerf_open(&dev, NULL);
    if (status != 0) {
        fprintf(stderr, "Failed to open bladeRF: %s\n", bladerf_strerror(status));
        return 1;
    }
    printf("Device opened in %.1f ms\n", now_ms() - t0);

    /* Print device info */
    struct bladerf_devinfo info;
    if (bladerf_get_devinfo(dev, &info) == 0) {
        printf("  Serial:  %s\n", info.serial);
        printf("  Product: %s\n", info.product);
    }

    const char *board = bladerf_get_board_name(dev);
    printf("  Board:   %s\n", board ? board : "unknown");
    printf("\n");

    /* ── Run benchmarks for each direction ── */
    for (int d = 0; d < num_dirs; d++) {
        const char *dir = directions[d];
        bladerf_channel ch = channels[d];
        int base = d * num_tests;

        printf("── %s channel ──\n", dir);

        /* Set initial state */
        CHECK(bladerf_set_frequency(dev, ch, FREQ_A));
        CHECK(bladerf_set_sample_rate(dev, ch, RATE_A, NULL));
        CHECK(bladerf_set_gain(dev, ch, GAIN_A));

        /* [1] Frequency retune */
        {
            bench_result_t *r = &results[base + 0];
            r->name = test_names[0];
            printf("  [1/7] Frequency (%.1f <-> %.1f MHz)... ",
                   FREQ_A / 1e6, FREQ_B / 1e6);
            fflush(stdout);
            for (int i = 0; i < n; i++) {
                bladerf_frequency target = (i % 2 == 0) ? FREQ_B : FREQ_A;
                double t = now_ms();
                CHECK(bladerf_set_frequency(dev, ch, target));
                r->times_ms[i] = now_ms() - t;
            }
            printf("avg=%.2f ms  min=%.2f ms  max=%.2f ms\n",
                   avg(r->times_ms, n), minv(r->times_ms, n), maxv(r->times_ms, n));
        }

        /* [2] Sample rate */
        {
            CHECK(bladerf_set_frequency(dev, ch, FREQ_A));
            bench_result_t *r = &results[base + 1];
            r->name = test_names[1];
            printf("  [2/7] Sample rate (%.1f <-> %.1f MSPS)... ",
                   RATE_A / 1e6, RATE_B / 1e6);
            fflush(stdout);
            for (int i = 0; i < n; i++) {
                bladerf_sample_rate target = (i % 2 == 0) ? RATE_B : RATE_A;
                double t = now_ms();
                CHECK(bladerf_set_sample_rate(dev, ch, target, NULL));
                r->times_ms[i] = now_ms() - t;
            }
            printf("avg=%.2f ms  min=%.2f ms  max=%.2f ms\n",
                   avg(r->times_ms, n), minv(r->times_ms, n), maxv(r->times_ms, n));
        }

        /* [3] Gain */
        {
            CHECK(bladerf_set_sample_rate(dev, ch, RATE_A, NULL));
            bench_result_t *r = &results[base + 2];
            r->name = test_names[2];
            printf("  [3/7] Gain (%d <-> %d dB)... ", GAIN_A, GAIN_B);
            fflush(stdout);
            for (int i = 0; i < n; i++) {
                bladerf_gain target = (i % 2 == 0) ? GAIN_B : GAIN_A;
                double t = now_ms();
                CHECK(bladerf_set_gain(dev, ch, target));
                r->times_ms[i] = now_ms() - t;
            }
            printf("avg=%.2f ms  min=%.2f ms  max=%.2f ms\n",
                   avg(r->times_ms, n), minv(r->times_ms, n), maxv(r->times_ms, n));
        }

        /* [4] Freq + rate (sequential) */
        {
            bench_result_t *r = &results[base + 3];
            r->name = test_names[3];
            printf("  [4/7] Freq + rate... ");
            fflush(stdout);
            for (int i = 0; i < n; i++) {
                bladerf_frequency f = (i % 2 == 0) ? FREQ_B : FREQ_A;
                bladerf_sample_rate sr = (i % 2 == 0) ? RATE_B : RATE_A;
                double t = now_ms();
                CHECK(bladerf_set_frequency(dev, ch, f));
                CHECK(bladerf_set_sample_rate(dev, ch, sr, NULL));
                r->times_ms[i] = now_ms() - t;
            }
            printf("avg=%.2f ms  min=%.2f ms  max=%.2f ms\n",
                   avg(r->times_ms, n), minv(r->times_ms, n), maxv(r->times_ms, n));
        }

        /* [5] Freq + rate + gain */
        {
            bench_result_t *r = &results[base + 4];
            r->name = test_names[4];
            printf("  [5/7] Freq + rate + gain... ");
            fflush(stdout);
            for (int i = 0; i < n; i++) {
                bladerf_frequency f = (i % 2 == 0) ? FREQ_B : FREQ_A;
                bladerf_sample_rate sr = (i % 2 == 0) ? RATE_B : RATE_A;
                bladerf_gain g = (i % 2 == 0) ? GAIN_B : GAIN_A;
                double t = now_ms();
                CHECK(bladerf_set_frequency(dev, ch, f));
                CHECK(bladerf_set_sample_rate(dev, ch, sr, NULL));
                CHECK(bladerf_set_gain(dev, ch, g));
                r->times_ms[i] = now_ms() - t;
            }
            printf("avg=%.2f ms  min=%.2f ms  max=%.2f ms\n",
                   avg(r->times_ms, n), minv(r->times_ms, n), maxv(r->times_ms, n));
        }

        /* [6] Same freq (no-op) */
        {
            CHECK(bladerf_set_frequency(dev, ch, FREQ_A));
            CHECK(bladerf_set_sample_rate(dev, ch, RATE_A, NULL));
            bench_result_t *r = &results[base + 5];
            r->name = test_names[5];
            printf("  [6/7] Same freq (no-op)... ");
            fflush(stdout);
            for (int i = 0; i < n; i++) {
                double t = now_ms();
                CHECK(bladerf_set_frequency(dev, ch, FREQ_A));
                r->times_ms[i] = now_ms() - t;
            }
            printf("avg=%.2f ms  min=%.2f ms  max=%.2f ms\n",
                   avg(r->times_ms, n), minv(r->times_ms, n), maxv(r->times_ms, n));
        }

        /* [7] 1 MHz hop */
        {
            bench_result_t *r = &results[base + 6];
            r->name = test_names[6];
            printf("  [7/7] 1 MHz freq hop... ");
            fflush(stdout);
            for (int i = 0; i < n; i++) {
                bladerf_frequency target = FREQ_A + (i % 5) * 1000000ULL;
                double t = now_ms();
                CHECK(bladerf_set_frequency(dev, ch, target));
                r->times_ms[i] = now_ms() - t;
            }
            printf("avg=%.2f ms  min=%.2f ms  max=%.2f ms\n",
                   avg(r->times_ms, n), minv(r->times_ms, n), maxv(r->times_ms, n));
        }

        printf("\n");
    }

    /* ── Summary table ── */
    printf("============================================================================================\n");
    printf("%-35s %10s %10s %10s %10s %6s\n",
           "Test", "Avg (ms)", "Min (ms)", "Max (ms)", "StdDev", "N");
    printf("--------------------------------------------------------------------------------------------\n");
    for (int d = 0; d < num_dirs; d++) {
        for (int t = 0; t < num_tests; t++) {
            bench_result_t *r = &results[d * num_tests + t];
            char name[64];
            snprintf(name, sizeof(name), "%s_native_%s", directions[d], r->name);
            printf("%-35s %10.2f %10.2f %10.2f %10.2f %6d\n",
                   name,
                   avg(r->times_ms, n), minv(r->times_ms, n),
                   maxv(r->times_ms, n), stddev(r->times_ms, n), n);
        }
    }
    printf("============================================================================================\n");

    /* ── Write CSV ── */
    csv = fopen("bladerf_native_bench.csv", "w");
    if (csv) {
        fprintf(csv, "test,iteration,time_ms\n");
        for (int d = 0; d < num_dirs; d++) {
            for (int t = 0; t < num_tests; t++) {
                bench_result_t *r = &results[d * num_tests + t];
                for (int i = 0; i < n; i++) {
                    fprintf(csv, "%s_native_%s,%d,%.4f\n",
                            directions[d], r->name, i, r->times_ms[i]);
                }
            }
        }
        fclose(csv);
        csv = NULL;
        printf("\nCSV written to bladerf_native_bench.csv\n");
    }

cleanup:
    if (dev) bladerf_close(dev);
    for (int i = 0; i < total_results; i++) free(results[i].times_ms);
    free(results);
    return 0;
}
