/*
 * bladerf_grid_bench.c — 10x10 grid benchmark for sample rate and frequency
 *
 * Measures latency of every (from → to) transition:
 *   Sample rate: [1,2,3,4,5,6,7,8,9,10] MHz  (10×10 = 100 cells × N runs)
 *   Frequency:   [100,200,...,1000] MHz        (10×10 = 100 cells × N runs)
 *
 * Build:  make grid
 * Run:    ./bladerf_grid_bench [runs_per_cell]    (default: 5)
 */

#include <stdio.h>
#include <stdlib.h>
#include <math.h>
#include <time.h>
#include <libbladeRF.h>

#define DEFAULT_RUNS     5
#define NUM_RATES       10
#define NUM_FREQS       10
#define INIT_GAIN       40

static const unsigned int RATES[NUM_RATES] = {
    1000000, 2000000, 3000000, 4000000, 5000000,
    6000000, 7000000, 8000000, 9000000, 10000000
};
static const int RATE_MHZ[NUM_RATES] = {1, 2, 3, 4, 5, 6, 7, 8, 9, 10};

static const uint64_t FREQS[NUM_FREQS] = {
    100000000ULL,  200000000ULL,  300000000ULL,  400000000ULL,  500000000ULL,
    600000000ULL,  700000000ULL,  800000000ULL,  900000000ULL, 1000000000ULL
};
static const int FREQ_MHZ[NUM_FREQS] = {100, 200, 300, 400, 500, 600, 700, 800, 900, 1000};

static double now_ms(void)
{
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ts.tv_sec * 1000.0 + ts.tv_nsec / 1e6;
}

/* Run one grid: sets param to from, then measures set(to), N times per cell.
 * Returns 0 on success. */
typedef int (*set_rate_fn)(struct bladerf *, bladerf_channel, bladerf_sample_rate,
                           bladerf_sample_rate *);
typedef int (*set_freq_fn)(struct bladerf *, bladerf_channel, bladerf_frequency);

static void bench_rate_grid(struct bladerf *dev, bladerf_channel ch,
                            int runs, FILE *csv)
{
    double avg_grid[NUM_RATES][NUM_RATES];

    printf("\n── Sample Rate Grid (TX) ──\n");
    printf("from\\to ");
    for (int j = 0; j < NUM_RATES; j++)
        printf(" %5dM", RATE_MHZ[j]);
    printf("\n");

    for (int i = 0; i < NUM_RATES; i++) {
        printf(" %4dM  ", RATE_MHZ[i]);
        fflush(stdout);

        for (int j = 0; j < NUM_RATES; j++) {
            double total = 0;
            int ok = 0;
            for (int r = 0; r < runs; r++) {
                /* Reset to from_rate */
                bladerf_set_sample_rate(dev, ch, RATES[i], NULL);
                /* Measure transition to to_rate */
                double t = now_ms();
                int s = bladerf_set_sample_rate(dev, ch, RATES[j], NULL);
                double elapsed = now_ms() - t;
                if (s == 0) {
                    fprintf(csv, "rate,%d,%d,%d,%.4f\n",
                            RATE_MHZ[i], RATE_MHZ[j], r, elapsed);
                    total += elapsed;
                    ok++;
                } else {
                    fprintf(csv, "rate,%d,%d,%d,-1\n",
                            RATE_MHZ[i], RATE_MHZ[j], r);
                }
            }
            avg_grid[i][j] = ok > 0 ? total / ok : -1;
            if (avg_grid[i][j] >= 0)
                printf(" %5.1f", avg_grid[i][j]);
            else
                printf("   ERR");
            fflush(stdout);
        }
        printf("\n");
    }
}

static void bench_freq_grid(struct bladerf *dev, bladerf_channel ch,
                            int runs, FILE *csv)
{
    double avg_grid[NUM_FREQS][NUM_FREQS];

    printf("\n── Frequency Grid (TX, at 4 MSPS) ──\n");
    printf("from\\to  ");
    for (int j = 0; j < NUM_FREQS; j++)
        printf(" %5dM", FREQ_MHZ[j]);
    printf("\n");

    for (int i = 0; i < NUM_FREQS; i++) {
        printf(" %5dM  ", FREQ_MHZ[i]);
        fflush(stdout);

        for (int j = 0; j < NUM_FREQS; j++) {
            double total = 0;
            int ok = 0;
            for (int r = 0; r < runs; r++) {
                /* Reset to from_freq */
                bladerf_set_frequency(dev, ch, FREQS[i]);
                /* Measure transition to to_freq */
                double t = now_ms();
                int s = bladerf_set_frequency(dev, ch, FREQS[j]);
                double elapsed = now_ms() - t;
                if (s == 0) {
                    fprintf(csv, "freq,%d,%d,%d,%.4f\n",
                            FREQ_MHZ[i], FREQ_MHZ[j], r, elapsed);
                    total += elapsed;
                    ok++;
                } else {
                    fprintf(csv, "freq,%d,%d,%d,-1\n",
                            FREQ_MHZ[i], FREQ_MHZ[j], r);
                }
            }
            avg_grid[i][j] = ok > 0 ? total / ok : -1;
            if (avg_grid[i][j] >= 0)
                printf(" %5.1f", avg_grid[i][j]);
            else
                printf("   ERR");
            fflush(stdout);
        }
        printf("\n");
    }
}

int main(int argc, char **argv)
{
    int runs = (argc > 1) ? atoi(argv[1]) : DEFAULT_RUNS;
    if (runs < 1) runs = DEFAULT_RUNS;

    struct bladerf *dev = NULL;

    printf("=== bladeRF Grid Benchmark (libbladeRF native) ===\n");
    printf("Runs per cell: %d\n", runs);
    printf("Rate grid:     %dx%d  [1..10 MHz]\n", NUM_RATES, NUM_RATES);
    printf("Freq grid:     %dx%d  [100..1000 MHz]\n", NUM_FREQS, NUM_FREQS);
    printf("Total calls:   ~%d  (rate) + ~%d  (freq)\n",
           NUM_RATES * NUM_RATES * runs * 2,
           NUM_FREQS * NUM_FREQS * runs * 2);
    printf("\n");

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

    /* Set initial state */
    bladerf_set_frequency(dev, ch, 2437000000ULL);
    bladerf_set_sample_rate(dev, ch, 1000000, NULL);
    bladerf_set_gain(dev, ch, INIT_GAIN);

    FILE *csv = fopen("bladerf_native_grid.csv", "w");
    if (!csv) { perror("fopen"); bladerf_close(dev); return 1; }
    fprintf(csv, "param,from,to,run,time_ms\n");

    double bench_start = now_ms();

    /* ── Rate grid ── */
    bench_rate_grid(dev, ch, runs, csv);

    /* Reset to 4 MSPS for freq grid */
    bladerf_set_sample_rate(dev, ch, 4000000, NULL);

    /* ── Freq grid ── */
    bench_freq_grid(dev, ch, runs, csv);

    double total_time = (now_ms() - bench_start) / 1000.0;
    printf("\nTotal benchmark time: %.1f s\n", total_time);

    fclose(csv);
    bladerf_close(dev);
    printf("CSV written to bladerf_native_grid.csv\n");
    return 0;
}
