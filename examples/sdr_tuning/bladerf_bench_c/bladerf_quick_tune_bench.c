/*
 * bladerf_quick_tune_bench.c — Quick-tune vs set_frequency benchmark
 *
 * Reproduces the frequency-only tests from bladerf_bench.c and
 * bladerf_grid_bench.c, comparing two methods:
 *   1. bladerf_set_frequency()      — full PLL recalculation each call
 *   2. bladerf_schedule_retune()    — pre-computed quick_tune parameters
 *
 * Tests (same frequencies as existing benchmarks):
 *   [1] Freq retune:  2.437 <-> 2.480 GHz  (from bladerf_bench)
 *   [2] Freq no-op:   same freq repeated   (from bladerf_bench)
 *   [3] Freq 1 MHz:   small hops           (from bladerf_bench)
 *   [4] Freq grid:    10×10 [100..1000 MHz] (from bladerf_grid_bench)
 *
 * Quick-tune pre-computes PLL divider/VCO settings once per frequency,
 * then applies them directly — skipping the expensive frequency planning.
 * Data stream is NOT interrupted (sample rate stays fixed).
 *
 * Build:  make bladerf_quick_tune_bench
 * Run:    ./bladerf_quick_tune_bench [iterations] [grid_runs_per_cell]
 */

#include <stdio.h>
#include <stdlib.h>
#include <math.h>
#include <time.h>
#include <libbladeRF.h>

#define DEFAULT_ITERATIONS    20
#define DEFAULT_GRID_RUNS      5

/* From bladerf_bench.c */
#define FREQ_A  2437000000ULL   /* 2.437 GHz */
#define FREQ_B  2480000000ULL   /* 2.480 GHz (+43 MHz) */

/* From bladerf_grid_bench.c */
#define NUM_GRID_FREQS  10
static const uint64_t GRID_FREQS[NUM_GRID_FREQS] = {
    100000000ULL,  200000000ULL,  300000000ULL,  400000000ULL,  500000000ULL,
    600000000ULL,  700000000ULL,  800000000ULL,  900000000ULL, 1000000000ULL
};
static const int GRID_MHZ[NUM_GRID_FREQS] = {100, 200, 300, 400, 500, 600, 700, 800, 900, 1000};

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

static void print_result(const char *name, const double *v, int n)
{
    printf("  %-30s avg=%8.2f  min=%8.2f  max=%8.2f  σ=%6.2f  ms\n",
           name, avg(v, n), minv(v, n), maxv(v, n), stddev(v, n));
}

int main(int argc, char **argv)
{
    int n = (argc > 1) ? atoi(argv[1]) : DEFAULT_ITERATIONS;
    int grid_runs = (argc > 2) ? atoi(argv[2]) : DEFAULT_GRID_RUNS;
    if (n < 1) n = DEFAULT_ITERATIONS;
    if (grid_runs < 1) grid_runs = DEFAULT_GRID_RUNS;

    struct bladerf *dev = NULL;
    FILE *csv = NULL;

    /* 3 simple tests × 2 methods = 6 result arrays */
    const int num_simple = 3;
    const char *test_names[] = {"freq", "freq_noop", "freq_1mhz"};
    bench_result_t set_freq_results[3];
    bench_result_t quick_tune_results[3];

    for (int i = 0; i < num_simple; i++) {
        set_freq_results[i].times_ms = calloc(n, sizeof(double));
        set_freq_results[i].count = n;
        set_freq_results[i].name = test_names[i];
        quick_tune_results[i].times_ms = calloc(n, sizeof(double));
        quick_tune_results[i].count = n;
        quick_tune_results[i].name = test_names[i];
    }

    /* Quick-tune params for simple tests */
    struct bladerf_quick_tune qt_a, qt_b;

    /* Quick-tune params for 1 MHz hops: FREQ_A + 0..4 MHz */
    struct bladerf_quick_tune qt_1mhz[5];

    /* Quick-tune params for grid */
    struct bladerf_quick_tune qt_grid[NUM_GRID_FREQS];

    printf("=== bladeRF Quick-Tune vs set_frequency Benchmark ===\n");
    printf("Simple tests: %d iterations  (2.437 <-> 2.480 GHz)\n", n);
    printf("Grid test:    %d runs/cell   (10×10, 100..1000 MHz)\n", grid_runs);
    printf("Sample rate:  4 MSPS (fixed, not changed)\n");
    printf("Data stream:  maintained (no dropout)\n");
    printf("\n");

    /* ── Open device ── */
    double t0 = now_ms();
    int status = bladerf_open(&dev, NULL);
    if (status != 0) {
        fprintf(stderr, "Failed to open bladeRF: %s\n", bladerf_strerror(status));
        return 1;
    }
    printf("Device opened in %.1f ms\n", now_ms() - t0);

    struct bladerf_devinfo info;
    if (bladerf_get_devinfo(dev, &info) == 0)
        printf("  Serial: %s  Product: %s\n", info.serial, info.product);
    const char *board = bladerf_get_board_name(dev);
    printf("  Board:  %s\n\n", board ? board : "unknown");

    bladerf_channel ch = BLADERF_CHANNEL_TX(0);

    /* Fixed sample rate throughout */
    CHECK(bladerf_set_sample_rate(dev, ch, 4000000, NULL));
    CHECK(bladerf_set_gain(dev, ch, 40));

    /* ── Pre-compute all quick_tune parameters ── */
    printf("── Pre-computing quick_tune parameters ──\n");

    CHECK(bladerf_set_frequency(dev, ch, FREQ_A));
    CHECK(bladerf_get_quick_tune(dev, ch, &qt_a));
    printf("  FREQ_A  %.3f MHz — OK\n", FREQ_A / 1e6);

    CHECK(bladerf_set_frequency(dev, ch, FREQ_B));
    CHECK(bladerf_get_quick_tune(dev, ch, &qt_b));
    printf("  FREQ_B  %.3f MHz — OK\n", FREQ_B / 1e6);

    for (int i = 0; i < 5; i++) {
        bladerf_frequency f = FREQ_A + (uint64_t)i * 1000000ULL;
        CHECK(bladerf_set_frequency(dev, ch, f));
        CHECK(bladerf_get_quick_tune(dev, ch, &qt_1mhz[i]));
        printf("  1MHz[%d] %.3f MHz — OK\n", i, f / 1e6);
    }

    for (int i = 0; i < NUM_GRID_FREQS; i++) {
        CHECK(bladerf_set_frequency(dev, ch, GRID_FREQS[i]));
        CHECK(bladerf_get_quick_tune(dev, ch, &qt_grid[i]));
        printf("  Grid[%d] %d MHz — OK\n", i, GRID_MHZ[i]);
    }
    printf("\n");

    /* ═══════════════════════════════════════════════════════════
     *  SIMPLE TESTS — bladerf_set_frequency
     * ═══════════════════════════════════════════════════════════ */
    printf("── bladerf_set_frequency ──\n");

    /* [1] Freq retune A <-> B */
    {
        CHECK(bladerf_set_frequency(dev, ch, FREQ_A));
        bench_result_t *r = &set_freq_results[0];
        for (int i = 0; i < n; i++) {
            bladerf_frequency target = (i % 2 == 0) ? FREQ_B : FREQ_A;
            double t = now_ms();
            CHECK(bladerf_set_frequency(dev, ch, target));
            r->times_ms[i] = now_ms() - t;
        }
        print_result("set_freq: freq", r->times_ms, n);
    }

    /* [2] Freq no-op */
    {
        CHECK(bladerf_set_frequency(dev, ch, FREQ_A));
        bench_result_t *r = &set_freq_results[1];
        for (int i = 0; i < n; i++) {
            double t = now_ms();
            CHECK(bladerf_set_frequency(dev, ch, FREQ_A));
            r->times_ms[i] = now_ms() - t;
        }
        print_result("set_freq: freq_noop", r->times_ms, n);
    }

    /* [3] 1 MHz hop */
    {
        bench_result_t *r = &set_freq_results[2];
        for (int i = 0; i < n; i++) {
            bladerf_frequency target = FREQ_A + (i % 5) * 1000000ULL;
            double t = now_ms();
            CHECK(bladerf_set_frequency(dev, ch, target));
            r->times_ms[i] = now_ms() - t;
        }
        print_result("set_freq: freq_1mhz", r->times_ms, n);
    }

    printf("\n");

    /* ═══════════════════════════════════════════════════════════
     *  SIMPLE TESTS — bladerf_schedule_retune (quick_tune)
     * ═══════════════════════════════════════════════════════════ */
    printf("── bladerf_schedule_retune (quick_tune) ──\n");

    /* [1] Freq retune A <-> B */
    {
        CHECK(bladerf_schedule_retune(dev, ch, BLADERF_RETUNE_NOW, 0, &qt_a));
        bench_result_t *r = &quick_tune_results[0];
        for (int i = 0; i < n; i++) {
            struct bladerf_quick_tune *qt = (i % 2 == 0) ? &qt_b : &qt_a;
            double t = now_ms();
            CHECK(bladerf_schedule_retune(dev, ch, BLADERF_RETUNE_NOW, 0, qt));
            r->times_ms[i] = now_ms() - t;
        }
        print_result("quick_tune: freq", r->times_ms, n);
    }

    /* [2] Freq no-op */
    {
        CHECK(bladerf_schedule_retune(dev, ch, BLADERF_RETUNE_NOW, 0, &qt_a));
        bench_result_t *r = &quick_tune_results[1];
        for (int i = 0; i < n; i++) {
            double t = now_ms();
            CHECK(bladerf_schedule_retune(dev, ch, BLADERF_RETUNE_NOW, 0, &qt_a));
            r->times_ms[i] = now_ms() - t;
        }
        print_result("quick_tune: freq_noop", r->times_ms, n);
    }

    /* [3] 1 MHz hop */
    {
        bench_result_t *r = &quick_tune_results[2];
        for (int i = 0; i < n; i++) {
            double t = now_ms();
            CHECK(bladerf_schedule_retune(dev, ch, BLADERF_RETUNE_NOW, 0,
                                          &qt_1mhz[i % 5]));
            r->times_ms[i] = now_ms() - t;
        }
        print_result("quick_tune: freq_1mhz", r->times_ms, n);
    }

    printf("\n");

    /* ═══════════════════════════════════════════════════════════
     *  SUMMARY TABLE — simple tests
     * ═══════════════════════════════════════════════════════════ */
    printf("════════════════════════════════════════════════════════════════════════════════════\n");
    printf("%-25s  %10s  %10s  %10s  %10s  %8s\n",
           "Test", "Avg (ms)", "Min (ms)", "Max (ms)", "StdDev", "Speedup");
    printf("────────────────────────────────────────────────────────────────────────────────────\n");
    for (int i = 0; i < num_simple; i++) {
        bench_result_t *sf = &set_freq_results[i];
        bench_result_t *qt = &quick_tune_results[i];
        double sf_avg = avg(sf->times_ms, n);
        double qt_avg = avg(qt->times_ms, n);
        printf("  set_freq %-14s  %10.2f  %10.2f  %10.2f  %10.2f\n",
               sf->name, sf_avg, minv(sf->times_ms, n),
               maxv(sf->times_ms, n), stddev(sf->times_ms, n));
        printf("  quick_tune %-12s  %10.2f  %10.2f  %10.2f  %10.2f  %7.1fx\n",
               qt->name, qt_avg, minv(qt->times_ms, n),
               maxv(qt->times_ms, n), stddev(qt->times_ms, n),
               sf_avg / qt_avg);
        printf("────────────────────────────────────────────────────────────────────────────────────\n");
    }

    /* ═══════════════════════════════════════════════════════════
     *  GRID TEST — 10×10 frequency grid
     * ═══════════════════════════════════════════════════════════ */
    double sf_grid[NUM_GRID_FREQS][NUM_GRID_FREQS];
    double qt_grid_avg[NUM_GRID_FREQS][NUM_GRID_FREQS];

    csv = fopen("bladerf_quick_tune_bench.csv", "w");
    if (!csv) { perror("fopen"); goto cleanup; }
    fprintf(csv, "method,test,from,to,run,time_ms\n");

    /* Write simple test results to CSV */
    for (int i = 0; i < num_simple; i++) {
        for (int j = 0; j < n; j++) {
            fprintf(csv, "set_frequency,%s,%d,%d,%d,%.4f\n",
                    test_names[i], 0, 0, j, set_freq_results[i].times_ms[j]);
            fprintf(csv, "schedule_retune,%s,%d,%d,%d,%.4f\n",
                    test_names[i], 0, 0, j, quick_tune_results[i].times_ms[j]);
        }
    }

    /* ── Grid: set_frequency ── */
    printf("\n── Running grid: set_frequency ──\n");
    for (int i = 0; i < NUM_GRID_FREQS; i++) {
        printf("  row %d/%d (%d MHz)...\r", i + 1, NUM_GRID_FREQS, GRID_MHZ[i]);
        fflush(stdout);
        for (int j = 0; j < NUM_GRID_FREQS; j++) {
            double total = 0;
            int ok = 0;
            for (int r = 0; r < grid_runs; r++) {
                bladerf_set_frequency(dev, ch, GRID_FREQS[i]);
                double t = now_ms();
                int s = bladerf_set_frequency(dev, ch, GRID_FREQS[j]);
                double elapsed = now_ms() - t;
                if (s == 0) {
                    fprintf(csv, "set_frequency,grid,%d,%d,%d,%.4f\n",
                            GRID_MHZ[i], GRID_MHZ[j], r, elapsed);
                    total += elapsed;
                    ok++;
                } else {
                    fprintf(csv, "set_frequency,grid,%d,%d,%d,-1\n",
                            GRID_MHZ[i], GRID_MHZ[j], r);
                }
            }
            sf_grid[i][j] = ok > 0 ? total / ok : -1;
        }
    }
    printf("  set_frequency grid done.       \n");

    /* ── Grid: schedule_retune (quick_tune) ── */
    printf("── Running grid: schedule_retune ──\n");
    for (int i = 0; i < NUM_GRID_FREQS; i++) {
        printf("  row %d/%d (%d MHz)...\r", i + 1, NUM_GRID_FREQS, GRID_MHZ[i]);
        fflush(stdout);
        for (int j = 0; j < NUM_GRID_FREQS; j++) {
            double total = 0;
            int ok = 0;
            for (int r = 0; r < grid_runs; r++) {
                bladerf_schedule_retune(dev, ch, BLADERF_RETUNE_NOW, 0,
                                        &qt_grid[i]);
                double t = now_ms();
                int s = bladerf_schedule_retune(dev, ch, BLADERF_RETUNE_NOW, 0,
                                                &qt_grid[j]);
                double elapsed = now_ms() - t;
                if (s == 0) {
                    fprintf(csv, "schedule_retune,grid,%d,%d,%d,%.4f\n",
                            GRID_MHZ[i], GRID_MHZ[j], r, elapsed);
                    total += elapsed;
                    ok++;
                } else {
                    fprintf(csv, "schedule_retune,grid,%d,%d,%d,-1\n",
                            GRID_MHZ[i], GRID_MHZ[j], r);
                }
            }
            qt_grid_avg[i][j] = ok > 0 ? total / ok : -1;
        }
    }
    printf("  schedule_retune grid done.     \n");

    fclose(csv);
    csv = NULL;
    printf("CSV written to bladerf_quick_tune_bench.csv\n");

    /* ═══════════════════════════════════════════════════════════
     *  SIDE-BY-SIDE GRIDS: set_frequency │ quick_tune │ speedup
     * ═══════════════════════════════════════════════════════════ */
    printf("\n");
    printf("═══════════════════════════════════════════════════════════════");
    printf("════════════════════════════════════════════════════════════════");
    printf("═══════════════════════════════════════════════════════════════\n");

    /* Headers */
    printf("   set_frequency (ms)            ");
    printf("          │  schedule_retune (ms)         ");
    printf("          │  speedup (×)                     \n");

    /* Column labels for each grid */
    printf("from\\to  ");
    for (int j = 0; j < NUM_GRID_FREQS; j++) printf(" %5dM", GRID_MHZ[j]);
    printf("  │  from\\to  ");
    for (int j = 0; j < NUM_GRID_FREQS; j++) printf(" %5dM", GRID_MHZ[j]);
    printf("  │  from\\to  ");
    for (int j = 0; j < NUM_GRID_FREQS; j++) printf(" %5dM", GRID_MHZ[j]);
    printf("\n");

    /* Rows */
    for (int i = 0; i < NUM_GRID_FREQS; i++) {
        /* set_frequency column */
        printf(" %5dM  ", GRID_MHZ[i]);
        for (int j = 0; j < NUM_GRID_FREQS; j++) {
            if (sf_grid[i][j] >= 0) printf(" %5.1f", sf_grid[i][j]);
            else                     printf("   ERR");
        }

        /* quick_tune column */
        printf("  │  %5dM  ", GRID_MHZ[i]);
        for (int j = 0; j < NUM_GRID_FREQS; j++) {
            if (qt_grid_avg[i][j] >= 0) printf(" %5.1f", qt_grid_avg[i][j]);
            else                         printf("   ERR");
        }

        /* speedup column */
        printf("  │  %5dM  ", GRID_MHZ[i]);
        for (int j = 0; j < NUM_GRID_FREQS; j++) {
            if (sf_grid[i][j] > 0 && qt_grid_avg[i][j] > 0)
                printf(" %5.1f", sf_grid[i][j] / qt_grid_avg[i][j]);
            else
                printf("     -");
        }
        printf("\n");
    }

    printf("═══════════════════════════════════════════════════════════════");
    printf("════════════════════════════════════════════════════════════════");
    printf("═══════════════════════════════════════════════════════════════\n");

cleanup:
    for (int i = 0; i < num_simple; i++) {
        free(set_freq_results[i].times_ms);
        free(quick_tune_results[i].times_ms);
    }
    if (csv) fclose(csv);
    if (dev) bladerf_close(dev);
    return 0;
}
