/*
 * freq_swap_qt.c — bladeRF quick-tune frequency-swap demo
 *
 * Mirrors examples/freq_swap/src/bin/freq_swap.rs but uses libbladeRF
 * directly with bladerf_get_quick_tune + bladerf_schedule_retune to do the
 * LO hops. RETUNE_NOW means we don't need the META sync format.
 *
 * Sequence (4 MSPS):
 *   t = 0.0..0.2 s : TX 828.500 MHz, RX 830 MHz   (tone at -1.5 MHz BB)
 *   t = 0.2..0.4 s : TX 831.500 MHz, RX 830 MHz   (tone at +1.5 MHz BB)
 *   t = 0.4..0.6 s : TX 831.500 MHz, RX 832 MHz   (tone at -0.5 MHz BB)
 *
 * Captures RX IQ to ./freq_swap.cf32 (interleaved float32 I,Q LE) and
 * metadata to ./freq_swap.meta.json so plot_freq_swap.py / inspectrum can
 * read the same files.
 *
 * Build: see Makefile.qt or run
 *   gcc -O2 -Wall -std=c11 -D_POSIX_C_SOURCE=200809L \
 *       freq_swap_qt.c -o freq_swap_qt -lbladeRF -lpthread
 */

#include <libbladeRF.h>

#include <pthread.h>
#include <stdatomic.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#define SAMPLE_RATE_HZ      4000000u
#define BANDWIDTH_HZ        4000000u

#define RX_CH               BLADERF_CHANNEL_RX(0)
#define TX_CH               BLADERF_CHANNEL_TX(1)

#define RX_START_FREQ_HZ    830000000ULL
#define TX_START_FREQ_HZ    828500000ULL
#define TX_TARGET_FREQ_HZ   831500000ULL
#define RX_TARGET_FREQ_HZ   832000000ULL

#define RX_GAIN_DB          0
#define TX_GAIN_DB          40

#define PRE_RETUNE_SECS     0.2
#define TX_ONLY_OBS_SECS    0.2
#define POST_RX_RETUNE_SECS 0.2

#define IQ_PATH             "freq_swap.cf32"
#define META_PATH           "freq_swap.meta.json"

#define SYNC_NUM_BUFFERS    64
#define SYNC_BUF_SPP        8192   /* sample-pairs per buffer */
#define SYNC_NUM_XFERS      16
#define SYNC_TIMEOUT_MS     1000

#define TX_AMPLITUDE        1448   /* sqrt(.5) * 2048 → |z|=1.0 in Q11 */

#define LOG_ERR(expr, status)                                                   \
    fprintf(stderr, "%s:%d %s: %s\n",                                           \
            __FILE__, __LINE__, (expr), bladerf_strerror(status))

#define CHECK_RET(expr) do {                                                    \
    int _s = (expr);                                                            \
    if (_s != 0) { LOG_ERR(#expr, _s); return -1; }                             \
} while (0)

#define CHECK_GOTO(expr) do {                                                   \
    int _s = (expr);                                                            \
    if (_s != 0) { LOG_ERR(#expr, _s); goto out; }                              \
} while (0)

struct tx_ctx {
    struct bladerf  *dev;
    atomic_int       stop;
};

static double now_secs(void)
{
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (double)ts.tv_sec + (double)ts.tv_nsec * 1e-9;
}

/* X2 TX, ch0 silent, ch1 carries a DC vector → CW carrier at TX_LO. */
static void *tx_worker(void *p)
{
    struct tx_ctx *ctx = p;
    const size_t spp = 4096;
    int16_t *buf = calloc(spp * 4, sizeof(int16_t));   /* 4 int16 per X2 SP */
    if (!buf) { perror("calloc"); return NULL; }
    for (size_t i = 0; i < spp; i++) {
        buf[4*i + 0] = 0;             /* ch0 I */
        buf[4*i + 1] = 0;             /* ch0 Q */
        buf[4*i + 2] = TX_AMPLITUDE;  /* ch1 I */
        buf[4*i + 3] = TX_AMPLITUDE;  /* ch1 Q */
    }
    while (!atomic_load(&ctx->stop)) {
        int s = bladerf_sync_tx(ctx->dev, buf, spp, NULL, SYNC_TIMEOUT_MS);
        if (s != 0) { LOG_ERR("bladerf_sync_tx", s); break; }
    }
    free(buf);
    return NULL;
}

static int configure_channel(struct bladerf *dev, bladerf_channel ch,
                             bladerf_frequency freq, bladerf_gain gain)
{
    CHECK_RET(bladerf_set_sample_rate(dev, ch, SAMPLE_RATE_HZ, NULL));
    CHECK_RET(bladerf_set_bandwidth(dev, ch, BANDWIDTH_HZ, NULL));
    CHECK_RET(bladerf_set_frequency(dev, ch, freq));
    CHECK_RET(bladerf_set_gain(dev, ch, gain));
    return 0;
}

/* Tune to `target`, snapshot the quick-tune state, then return to `restore`. */
static int cache_quick_tune(struct bladerf *dev, bladerf_channel ch,
                            bladerf_frequency target,
                            bladerf_frequency restore,
                            struct bladerf_quick_tune *qt)
{
    CHECK_RET(bladerf_set_frequency(dev, ch, target));
    CHECK_RET(bladerf_get_quick_tune(dev, ch, qt));
    CHECK_RET(bladerf_set_frequency(dev, ch, restore));
    return 0;
}

int main(void)
{
    struct bladerf *dev = NULL;
    pthread_t tx_thr;
    int tx_thr_started = 0;
    struct tx_ctx ctx = { .dev = NULL };
    atomic_init(&ctx.stop, 0);
    FILE *fp = NULL;
    int16_t *rx_buf = NULL;
    float   *cf32_buf = NULL;
    double tx_retune_t = 0.0, rx_retune_t = 0.0;
    int rc = 1;

    bladerf_log_set_verbosity(BLADERF_LOG_LEVEL_INFO);

    CHECK_GOTO(bladerf_open(&dev, NULL));
    ctx.dev = dev;

    fprintf(stderr, "Opened bladeRF: %s\n", bladerf_get_board_name(dev));

    if (configure_channel(dev, TX_CH, TX_START_FREQ_HZ, TX_GAIN_DB) != 0) goto out;
    if (configure_channel(dev, RX_CH, RX_START_FREQ_HZ, RX_GAIN_DB) != 0) goto out;

    /* Cache quick-tune snapshots for the two retune targets. */
    struct bladerf_quick_tune tx_target_qt;
    struct bladerf_quick_tune rx_target_qt;
    if (cache_quick_tune(dev, TX_CH, TX_TARGET_FREQ_HZ, TX_START_FREQ_HZ, &tx_target_qt) != 0) goto out;
    if (cache_quick_tune(dev, RX_CH, RX_TARGET_FREQ_HZ, RX_START_FREQ_HZ, &rx_target_qt) != 0) goto out;
    fprintf(stderr,
            "Quick-tune cached: TX→%.3f MHz (rffe_profile=%u, nios_profile=%u), "
            "RX→%.3f MHz (rffe_profile=%u, nios_profile=%u)\n",
            TX_TARGET_FREQ_HZ / 1e6,
            tx_target_qt.rffe_profile, tx_target_qt.nios_profile,
            RX_TARGET_FREQ_HZ / 1e6,
            rx_target_qt.rffe_profile, rx_target_qt.nios_profile);

    /* Non-meta sync config: RETUNE_NOW does not require the META timestamp
     * format (matches host/libraries/libbladeRF_test/test_quick_retune). */
    CHECK_GOTO(bladerf_sync_config(dev, BLADERF_TX_X2, BLADERF_FORMAT_SC16_Q11,
                                   SYNC_NUM_BUFFERS, SYNC_BUF_SPP,
                                   SYNC_NUM_XFERS, SYNC_TIMEOUT_MS));
    CHECK_GOTO(bladerf_sync_config(dev, BLADERF_RX_X1, BLADERF_FORMAT_SC16_Q11,
                                   SYNC_NUM_BUFFERS, SYNC_BUF_SPP,
                                   SYNC_NUM_XFERS, SYNC_TIMEOUT_MS));

    /* X2 TX requires both channels enabled. */
    CHECK_GOTO(bladerf_enable_module(dev, BLADERF_CHANNEL_TX(0), true));
    CHECK_GOTO(bladerf_enable_module(dev, TX_CH, true));
    CHECK_GOTO(bladerf_enable_module(dev, RX_CH, true));

    if (pthread_create(&tx_thr, NULL, tx_worker, &ctx) != 0) {
        perror("pthread_create");
        goto out;
    }
    tx_thr_started = 1;

    fp = fopen(IQ_PATH, "wb");
    if (!fp) { perror(IQ_PATH); goto out; }

    const size_t rx_spp = 4096;
    rx_buf  = malloc(rx_spp * 2 * sizeof(int16_t));   /* X1: 2 int16/SP */
    cf32_buf = malloc(rx_spp * 2 * sizeof(float));
    if (!rx_buf || !cf32_buf) { perror("malloc"); goto out; }

    fprintf(stderr,
            "Schedule:\n"
            "  TX %.3f → %.3f MHz at +%.2fs (quick_tune)\n"
            "  RX %.3f → %.3f MHz at +%.2fs (quick_tune)\n"
            "  stop at +%.2fs\n",
            TX_START_FREQ_HZ / 1e6, TX_TARGET_FREQ_HZ / 1e6, PRE_RETUNE_SECS,
            RX_START_FREQ_HZ / 1e6, RX_TARGET_FREQ_HZ / 1e6,
            PRE_RETUNE_SECS + TX_ONLY_OBS_SECS,
            PRE_RETUNE_SECS + TX_ONLY_OBS_SECS + POST_RX_RETUNE_SECS);

    const double t0 = now_secs();
    const double t_tx_retune = t0 + PRE_RETUNE_SECS;
    const double t_rx_retune = t_tx_retune + TX_ONLY_OBS_SECS;
    const double t_stop      = t_rx_retune + POST_RX_RETUNE_SECS;
    bool tx_done = false, rx_done = false;

    while (1) {
        const double now = now_secs();
        if (!tx_done && now >= t_tx_retune) {
            int s = bladerf_schedule_retune(dev, TX_CH, BLADERF_RETUNE_NOW, 0,
                                            &tx_target_qt);
            tx_retune_t = now - t0;
            if (s != 0) { LOG_ERR("schedule_retune TX", s); goto out; }
            fprintf(stderr, "[t=%.4fs] TX quick-tuned → %.3f MHz\n",
                    tx_retune_t, TX_TARGET_FREQ_HZ / 1e6);
            tx_done = true;
        }
        if (!rx_done && now >= t_rx_retune) {
            int s = bladerf_schedule_retune(dev, RX_CH, BLADERF_RETUNE_NOW, 0,
                                            &rx_target_qt);
            rx_retune_t = now - t0;
            if (s != 0) { LOG_ERR("schedule_retune RX", s); goto out; }
            fprintf(stderr, "[t=%.4fs] RX quick-tuned → %.3f MHz\n",
                    rx_retune_t, RX_TARGET_FREQ_HZ / 1e6);
            rx_done = true;
        }
        if (now >= t_stop) break;

        int s = bladerf_sync_rx(dev, rx_buf, rx_spp, NULL, SYNC_TIMEOUT_MS);
        if (s != 0) { LOG_ERR("bladerf_sync_rx", s); goto out; }
        for (size_t i = 0; i < rx_spp * 2; i++) {
            cf32_buf[i] = (float)rx_buf[i] / 2048.0f;
        }
        if (fwrite(cf32_buf, sizeof(float), rx_spp * 2, fp) != rx_spp * 2) {
            perror("fwrite");
            goto out;
        }
    }

    fprintf(stderr, "[t=%.4fs] stopping\n", now_secs() - t0);
    rc = 0;

out:
    if (tx_thr_started) {
        atomic_store(&ctx.stop, 1);
        pthread_join(tx_thr, NULL);
    }
    if (dev) {
        bladerf_enable_module(dev, TX_CH, false);
        bladerf_enable_module(dev, BLADERF_CHANNEL_TX(0), false);
        bladerf_enable_module(dev, RX_CH, false);
    }
    if (fp) fclose(fp);
    free(rx_buf);
    free(cf32_buf);

    if (rc == 0) {
        FILE *m = fopen(META_PATH, "w");
        if (m) {
            fprintf(m,
                    "{\n"
                    "  \"sample_rate_hz\": %u,\n"
                    "  \"rx_start_freq_hz\": %llu,\n"
                    "  \"tx_start_freq_hz\": %llu,\n"
                    "  \"tx_target_freq_hz\": %llu,\n"
                    "  \"rx_target_freq_hz\": %llu,\n"
                    "  \"tx_retune_t_s\": %.6f,\n"
                    "  \"rx_retune_t_s\": %.6f,\n"
                    "  \"capture_total_s\": %.6f,\n"
                    "  \"iq_path\": \"%s\",\n"
                    "  \"format\": \"cf32 (interleaved float32 I,Q little-endian)\",\n"
                    "  \"backend\": \"libbladeRF + quick_tune\"\n"
                    "}\n",
                    SAMPLE_RATE_HZ,
                    (unsigned long long)RX_START_FREQ_HZ,
                    (unsigned long long)TX_START_FREQ_HZ,
                    (unsigned long long)TX_TARGET_FREQ_HZ,
                    (unsigned long long)RX_TARGET_FREQ_HZ,
                    tx_retune_t, rx_retune_t,
                    PRE_RETUNE_SECS + TX_ONLY_OBS_SECS + POST_RX_RETUNE_SECS,
                    IQ_PATH);
            fclose(m);
            fprintf(stderr, "Wrote %s\n", META_PATH);
        } else {
            perror(META_PATH);
        }
    }

    if (dev) bladerf_close(dev);
    return rc;
}
