/*
 * retune_timing.c — Native libbladeRF analogue of `retune_timing.rs`.
 *
 * Sibling of ../src/bin/retune_timing.rs (FutureSDR + seify version).
 * Records an RX stream while performing TWO frequency changes mid-stream:
 *
 *   Event 1 — QUICK TUNE  (bladerf_schedule_retune + cached qt params)
 *             at t = --switch-ms,        target = --target-freq-hz
 *
 *   Event 2 — REGULAR     (bladerf_set_frequency)
 *             at t = --switch-ms-slow,   target = target + --slow-offset-hz
 *
 * Each event places a distinct marker pulse in the IQ stream so both can
 * be located in the recording:
 *
 *   marker 1 (quick): I = +mark_amp, Q = 0          → spike on I axis
 *   marker 2 (slow):  I = 0,         Q = +mark_amp  → spike on Q axis
 *
 * The hardware-retune latency for each method = (samples from its marker
 * to the visible frequency-shift transient that follows it). Comparing
 * the two numbers in a single capture isolates the speed of the bladeRF
 * "quick tune" path vs the full PLL recalibration path — temperature/
 * environment held constant.
 *
 * Streaming uses BLADERF_FORMAT_SC16_Q11_META (required by
 * bladerf_schedule_retune). Samples are converted to cf32 on the host
 * and written to <iq_path> — same format as the Rust retune_timing.rs,
 * so plot_retune_timing.py reads the capture.
 *
 * Build:  make
 *   or:   gcc -O2 -o retune_timing retune_timing.c -lbladeRF -lm
 *
 * Run (defaults: 4 MS/s, hop to target at t=80ms, then +1 MHz at t=160ms):
 *   ./retune_timing
 *   ./retune_timing --start-freq-hz 2425000000 --target-freq-hz 2450000000
 *
 * Output:
 *   retune_timing.cf32       — interleaved float32 I,Q
 *   retune_timing.meta.json  — sample rate, both event timestamps + indices
 */

/* Feature-test macro: needed for clock_gettime / CLOCK_MONOTONIC under
 * strict-conformance libc setups (and to silence IDE C parsers that
 * default to a pre-POSIX-1993 visibility). Must precede every system
 * header include. */
#define _POSIX_C_SOURCE 200809L

#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>
#include <string.h>
#include <stdbool.h>
#include <time.h>
#include <getopt.h>
#include <math.h>
#include <libbladeRF.h>

#define SC16_Q11_SCALE  2048.0f

#define DEFAULT_BUFFER_SIZE   1024u
#define DEFAULT_NUM_BUFFERS   16u
#define DEFAULT_NUM_TRANSFERS 8u
#define DEFAULT_STREAM_TIMEOUT_MS 1000u

static double now_s(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (double)ts.tv_sec + (double)ts.tv_nsec / 1e9;
}

#define CHECK(call) do {                                                   \
    int _s = (call);                                                       \
    if (_s != 0) {                                                         \
        fprintf(stderr, "ERROR: %s → %s\n", #call, bladerf_strerror(_s));  \
        rc = 1;                                                            \
        goto cleanup;                                                      \
    }                                                                      \
} while (0)

typedef struct {
    const char *device_args;
    unsigned int channel;
    double sample_rate;
    double start_freq_hz;
    double target_freq_hz;       /* first hop (quick tune) */
    double slow_offset_hz;       /* second hop = target + this */
    double switch_ms;            /* time of first (quick) retune */
    double switch_ms_slow;       /* time of second (slow) retune */
    double total_ms;
    int gain_db;
    float mark_amp;
    unsigned int mark_samples;
    const char *iq_path;
    const char *meta_path;
    unsigned int buffer_size;
} args_t;

static void print_usage(const char *prog) {
    printf(
"Usage: %s [options]\n"
"  --device <args>           bladeRF device identifier (default: \"\" — first available)\n"
"  --channel <n>             RX channel index (default: 0)\n"
"  --sample-rate <Hz>        sample rate (default: 4000000)\n"
"  --start-freq-hz <Hz>      initial RX frequency (default: 830000000)\n"
"  --target-freq-hz <Hz>     target for QUICK TUNE event (default: 830100000)\n"
"  --slow-offset-hz <Hz>     SLOW event freq = target + offset (default: 1000000 = +1 MHz)\n"
"  --switch-ms <ms>          time of QUICK TUNE event (default: 80)\n"
"  --switch-ms-slow <ms>     time of SLOW (set_frequency) event (default: 160)\n"
"  --total-ms <ms>           total capture duration (default: 250)\n"
"  --gain-db <dB>            RX gain (default: 30)\n"
"  --mark-amp <amp>          marker amplitude in cf32 units (default: 10)\n"
"  --mark-samples <n>        marker pulse length in samples (default: 8)\n"
"  --buffer-size <n>         sync_rx buffer size in samples (default: 1024)\n"
"  --iq-path <path>          output IQ file (default: retune_timing.cf32)\n"
"  --meta-path <path>        output metadata JSON (default: retune_timing.meta.json)\n"
"  -h, --help                this help\n", prog);
}

static int parse_args(int argc, char **argv, args_t *a) {
    a->device_args     = "";
    a->channel         = 0;
    a->sample_rate     = 8000000.0;
    a->start_freq_hz   = 830000000.0;
    a->target_freq_hz  = 831000000.0;
    a->slow_offset_hz  = 1000000.0;
    a->switch_ms       = 80.0;
    a->switch_ms_slow  = 160.0;
    a->total_ms        = 250.0;
    a->gain_db         = 0;
    a->mark_amp        = 10.0f;
    a->mark_samples    = 8;
    a->iq_path         = "retune_timing.cf32";
    a->meta_path       = "retune_timing.meta.json";
    a->buffer_size     = DEFAULT_BUFFER_SIZE;

    static struct option opts[] = {
        {"device",          required_argument, 0, 'd'},
        {"channel",         required_argument, 0, 'c'},
        {"sample-rate",     required_argument, 0, 's'},
        {"start-freq-hz",   required_argument, 0, 'f'},
        {"target-freq-hz",  required_argument, 0, 't'},
        {"slow-offset-hz",  required_argument, 0, 'O'},
        {"switch-ms",       required_argument, 0, 'w'},
        {"switch-ms-slow",  required_argument, 0, 'W'},
        {"total-ms",        required_argument, 0, 'T'},
        {"gain-db",         required_argument, 0, 'g'},
        {"mark-amp",        required_argument, 0, 'a'},
        {"mark-samples",    required_argument, 0, 'm'},
        {"buffer-size",     required_argument, 0, 'b'},
        {"iq-path",         required_argument, 0, 'i'},
        {"meta-path",       required_argument, 0, 'M'},
        {"help",            no_argument,       0, 'h'},
        {0, 0, 0, 0},
    };

    int c;
    while ((c = getopt_long(argc, argv, "d:c:s:f:t:O:w:W:T:g:a:m:b:i:M:h", opts, NULL)) != -1) {
        switch (c) {
            case 'd': a->device_args     = optarg; break;
            case 'c': a->channel         = (unsigned)atoi(optarg); break;
            case 's': a->sample_rate     = atof(optarg); break;
            case 'f': a->start_freq_hz   = atof(optarg); break;
            case 't': a->target_freq_hz  = atof(optarg); break;
            case 'O': a->slow_offset_hz  = atof(optarg); break;
            case 'w': a->switch_ms       = atof(optarg); break;
            case 'W': a->switch_ms_slow  = atof(optarg); break;
            case 'T': a->total_ms        = atof(optarg); break;
            case 'g': a->gain_db         = atoi(optarg); break;
            case 'a': a->mark_amp        = (float)atof(optarg); break;
            case 'm': a->mark_samples    = (unsigned)atoi(optarg); break;
            case 'b': a->buffer_size     = (unsigned)atoi(optarg); break;
            case 'i': a->iq_path         = optarg; break;
            case 'M': a->meta_path       = optarg; break;
            case 'h': print_usage(argv[0]); return 1;
            default:  print_usage(argv[0]); return 2;
        }
    }

    if (a->switch_ms_slow <= a->switch_ms) {
        fprintf(stderr, "ERROR: --switch-ms-slow (%.1f) must be > --switch-ms (%.1f)\n",
                a->switch_ms_slow, a->switch_ms);
        return 2;
    }
    if (a->total_ms <= a->switch_ms_slow) {
        fprintf(stderr, "ERROR: --total-ms (%.1f) must be > --switch-ms-slow (%.1f)\n",
                a->total_ms, a->switch_ms_slow);
        return 2;
    }
    return 0;
}

int main(int argc, char **argv) {
    args_t a;
    int parse_rc = parse_args(argc, argv, &a);
    if (parse_rc != 0) return (parse_rc == 1) ? 0 : 2;

    int rc = 0;
    struct bladerf *dev = NULL;
    int16_t *sc16_buf  = NULL;
    float   *cf32_buf  = NULL;
    FILE    *iq_fp     = NULL;
    FILE    *meta_fp   = NULL;

    bladerf_channel ch = BLADERF_CHANNEL_RX(a.channel);
    double slow_target_hz = a.target_freq_hz + a.slow_offset_hz;

    /* ── Banner ── */
    printf("=== retune_timing (libbladeRF direct — quick tune vs set_frequency) ===\n");
    printf("  device:         \"%s\"\n", a.device_args);
    printf("  RX ch%u:        %.3f MS/s, gain %d dB\n",
           a.channel, a.sample_rate / 1e6, a.gain_db);
    printf("  start freq:     %.6f MHz\n", a.start_freq_hz / 1e6);
    printf("  event 1 (QUICK): %.6f MHz   at t = %.3f ms\n",
           a.target_freq_hz / 1e6, a.switch_ms);
    printf("  event 2 (SLOW):  %.6f MHz   at t = %.3f ms  (Δ = %+.3f MHz vs event 1)\n",
           slow_target_hz / 1e6, a.switch_ms_slow, a.slow_offset_hz / 1e6);
    printf("  marker 1 (Q):    (%.2f, 0)   × %u samples → spike on I\n",
           a.mark_amp, a.mark_samples);
    printf("  marker 2 (S):    (0, %.2f)   × %u samples → spike on Q\n",
           a.mark_amp, a.mark_samples);
    printf("  total:           %.3f ms\n", a.total_ms);
    printf("  buffer:          %u samples (~%.2f µs per sync_rx)\n",
           a.buffer_size, (double)a.buffer_size / a.sample_rate * 1e6);
    printf("  IQ → %s, meta → %s\n", a.iq_path, a.meta_path);

    /* ── Open ── */
    double t_open0 = now_s();
    CHECK(bladerf_open(&dev, a.device_args));
    printf("  opened in %.3f ms\n", (now_s() - t_open0) * 1e3);

    struct bladerf_devinfo info;
    if (bladerf_get_devinfo(dev, &info) == 0) {
        printf("    serial:  %s\n", info.serial);
        printf("    product: %s\n", info.product);
    }
    const char *board = bladerf_get_board_name(dev);
    printf("    board:   %s\n", board ? board : "unknown");

    /* ── Configure RX ── */
    bladerf_sample_rate actual_rate = 0;
    CHECK(bladerf_set_sample_rate(dev, ch, (bladerf_sample_rate)a.sample_rate, &actual_rate));
    if ((double)actual_rate != a.sample_rate) {
        printf("  note: sample_rate clamped: requested %.3f MS/s, actual %.3f MS/s\n",
               a.sample_rate / 1e6, (double)actual_rate / 1e6);
        a.sample_rate = (double)actual_rate;
    }
    CHECK(bladerf_set_gain(dev, ch, (bladerf_gain)a.gain_db));

    /* ── Pre-tune to learn QUICK TUNE parameters for the target frequency. ──
     * Canonical flow (per libbladeRF's quick_tune.c example):
     *   1. Slow-tune to target_freq → PLL/RFIC settle
     *   2. bladerf_get_quick_tune  → snapshot register state for target
     *   3. Slow-tune back to start_freq → ready to begin streaming on start
     * Later, schedule_retune(qt, BLADERF_RETUNE_NOW, freq=0) applies the
     * cached state to switch immediately.
     */
    printf("\n[pre-tune] slow-tune to target %.6f MHz to learn quick-tune params...\n",
           a.target_freq_hz / 1e6);
    CHECK(bladerf_set_frequency(dev, ch, (bladerf_frequency)a.target_freq_hz));

    struct bladerf_quick_tune qt_target;
    CHECK(bladerf_get_quick_tune(dev, ch, &qt_target));
    printf("[pre-tune] captured quick_tune for %.6f MHz\n", a.target_freq_hz / 1e6);

    CHECK(bladerf_set_frequency(dev, ch, (bladerf_frequency)a.start_freq_hz));
    printf("[pre-tune] reset to start %.6f MHz\n", a.start_freq_hz / 1e6);

    /* ── Sync config (META format is required by schedule_retune). ── */
    CHECK(bladerf_sync_config(
        dev,
        BLADERF_RX_X1,
        BLADERF_FORMAT_SC16_Q11_META,
        DEFAULT_NUM_BUFFERS,
        a.buffer_size,
        DEFAULT_NUM_TRANSFERS,
        DEFAULT_STREAM_TIMEOUT_MS));

    /* ── Buffers ── */
    sc16_buf = malloc(sizeof(int16_t) * 2 * a.buffer_size);
    cf32_buf = malloc(sizeof(float)   * 2 * a.buffer_size);
    if (!sc16_buf || !cf32_buf) {
        fprintf(stderr, "out of memory\n");
        rc = 1;
        goto cleanup;
    }

    iq_fp = fopen(a.iq_path, "wb");
    if (!iq_fp) {
        fprintf(stderr, "cannot open %s for writing\n", a.iq_path);
        rc = 1;
        goto cleanup;
    }

    /* ── Start streaming ── */
    CHECK(bladerf_enable_module(dev, ch, true));

    /* Marker amplitudes in SC16 (Q11 full scale = ±2047; clamp). */
    int16_t mark_i16 = (int16_t)fminf(a.mark_amp * SC16_Q11_SCALE, 32767.0f);

    /* Timestamps + event bookkeeping. */
    double t0          = now_s();
    double switch_at_s = a.switch_ms / 1e3;
    double slow_at_s   = a.switch_ms_slow / 1e3;
    double total_s     = a.total_ms / 1e3;

    bool   quick_done  = false;
    bool   slow_done   = false;
    double t_quick_mark_s = -1.0, t_quick_fire_s = -1.0, t_quick_ack_s = -1.0;
    double t_slow_mark_s  = -1.0, t_slow_fire_s  = -1.0, t_slow_ack_s  = -1.0;
    uint64_t quick_mark_idx = 0;
    uint64_t slow_mark_idx  = 0;
    uint64_t total_samples_written = 0;

    printf("\nstreaming...\n");

    /* META format is required by schedule_retune (and we configured it
     * above). For *receive*, we don't care about timestamps — we just want
     * "give me samples as they arrive". The driver requires
     * BLADERF_META_FLAG_RX_NOW for that mode; without it sync_rx waits
     * for `timestamp` (== 0 here, which is in the past) and errors. */
    struct bladerf_metadata meta;
    memset(&meta, 0, sizeof(meta));
    meta.flags = BLADERF_META_FLAG_RX_NOW;

    while (true) {
        int s = bladerf_sync_rx(dev, sc16_buf, a.buffer_size, &meta, DEFAULT_STREAM_TIMEOUT_MS);
        if (s != 0) {
            fprintf(stderr, "bladerf_sync_rx: %s (status=0x%x, actual_count=%u)\n",
                    bladerf_strerror(s), meta.status, meta.actual_count);
            rc = 1;
            break;
        }
        double t_rel = now_s() - t0;

        /* ── Event 1: QUICK TUNE ──
         * Insert marker 1 (I-spike) into the start of this buffer, then
         * fire bladerf_schedule_retune with the pre-captured qt params and
         * BLADERF_RETUNE_NOW. The marker lands in the file at the same
         * sample position where we requested the quick retune; later
         * samples show the frequency-shift transient — the gap measures
         * the quick-tune hardware latency.
         */
        if (!quick_done && t_rel >= switch_at_s) {
            unsigned n_mark = a.mark_samples;
            if (n_mark > a.buffer_size) n_mark = a.buffer_size;
            for (unsigned i = 0; i < n_mark; i++) {
                sc16_buf[2 * i + 0] = mark_i16;  /* I = +mark_amp */
                sc16_buf[2 * i + 1] = 0;          /* Q = 0 */
            }
            quick_mark_idx = total_samples_written;
            t_quick_mark_s = t_rel;

            t_quick_fire_s = now_s() - t0;
            /* freq=0 is the canonical placeholder when quick_tune is non-NULL
             * (per libbladeRF/host/.../examples/quick_tune.c). The qt struct
             * carries the target — passing the explicit freq here would be
             * ignored. */
            int rs = bladerf_schedule_retune(
                dev, ch, BLADERF_RETUNE_NOW, 0, &qt_target);
            t_quick_ack_s = now_s() - t0;
            if (rs != 0) {
                fprintf(stderr, "bladerf_schedule_retune (quick): %s\n", bladerf_strerror(rs));
                rc = 1;
                break;
            }
            printf("[t=%.6fs] EVENT 1 QUICK → %.6f MHz   marker @ sample %lu   "
                   "(call Δ = %.3f ms)\n",
                   t_quick_mark_s,
                   a.target_freq_hz / 1e6,
                   (unsigned long)quick_mark_idx,
                   (t_quick_ack_s - t_quick_fire_s) * 1e3);
            quick_done = true;
        }

        /* ── Event 2: REGULAR set_frequency ──
         * Same idea but marker 2 is a Q-spike (orthogonal) so the plot
         * script can distinguish the two events. set_frequency does the
         * full PLL recalibration path.
         */
        if (quick_done && !slow_done && t_rel >= slow_at_s) {
            unsigned n_mark = a.mark_samples;
            if (n_mark > a.buffer_size) n_mark = a.buffer_size;
            for (unsigned i = 0; i < n_mark; i++) {
                sc16_buf[2 * i + 0] = 0;          /* I = 0 */
                sc16_buf[2 * i + 1] = mark_i16;   /* Q = +mark_amp */
            }
            slow_mark_idx = total_samples_written;
            t_slow_mark_s = t_rel;

            t_slow_fire_s = now_s() - t0;
            int rs = bladerf_set_frequency(dev, ch, (bladerf_frequency)slow_target_hz);
            t_slow_ack_s = now_s() - t0;
            if (rs != 0) {
                fprintf(stderr, "bladerf_set_frequency (slow): %s\n", bladerf_strerror(rs));
                rc = 1;
                break;
            }
            printf("[t=%.6fs] EVENT 2 SLOW  → %.6f MHz   marker @ sample %lu   "
                   "(call Δ = %.3f ms)\n",
                   t_slow_mark_s,
                   slow_target_hz / 1e6,
                   (unsigned long)slow_mark_idx,
                   (t_slow_ack_s - t_slow_fire_s) * 1e3);
            slow_done = true;
        }

        /* SC16_Q11 → cf32, then write. */
        const float scale = 1.0f / SC16_Q11_SCALE;
        for (unsigned i = 0; i < 2 * a.buffer_size; i++) {
            cf32_buf[i] = (float)sc16_buf[i] * scale;
        }
        size_t written = fwrite(cf32_buf, sizeof(float), 2 * a.buffer_size, iq_fp);
        if (written != 2 * a.buffer_size) {
            fprintf(stderr, "short write to %s\n", a.iq_path);
            rc = 1;
            break;
        }
        total_samples_written += a.buffer_size;

        if (t_rel >= total_s) break;
    }

    {
        int s = bladerf_enable_module(dev, ch, false);
        if (s != 0) fprintf(stderr, "warn: bladerf_enable_module(false): %s\n", bladerf_strerror(s));
    }
    if (iq_fp) { fflush(iq_fp); fclose(iq_fp); iq_fp = NULL; }

    double t_stop_s = now_s() - t0;
    printf("[t=%.6fs] stopped. Wrote %lu samples (%.3f s of capture).\n",
           t_stop_s,
           (unsigned long)total_samples_written,
           (double)total_samples_written / a.sample_rate);

    /* ── Metadata JSON ── */
    meta_fp = fopen(a.meta_path, "w");
    if (!meta_fp) {
        fprintf(stderr, "cannot open %s for writing\n", a.meta_path);
        rc = 1;
    } else {
        fprintf(meta_fp,
            "{\n"
            "  \"source\": \"libbladeRF direct (no seify/FutureSDR) — quick-tune vs set_frequency\",\n"
            "  \"device\": \"%s\",\n"
            "  \"channel\": %u,\n"
            "  \"sample_rate_hz\": %.6f,\n"
            "  \"start_freq_hz\": %.6f,\n"
            "  \"target_freq_hz\": %.6f,\n"
            "  \"slow_target_freq_hz\": %.6f,\n"
            "  \"slow_offset_hz\": %.6f,\n"
            "  \"switch_ms\": %.6f,\n"
            "  \"switch_ms_slow\": %.6f,\n"
            "  \"gain_db\": %d,\n"
            "  \"mark_amp\": %.6f,\n"
            "  \"mark_samples\": %u,\n"
            "  \"capture_total_ms\": %.6f,\n"
            "  \"events\": [\n"
            "    {\n"
            "      \"label\": \"quick_tune\",\n"
            "      \"method\": \"bladerf_schedule_retune + cached quick_tune\",\n"
            "      \"target_freq_hz\": %.6f,\n"
            "      \"marker_axis\": \"I\",\n"
            "      \"marker_sample_idx\": %lu,\n"
            "      \"t_mark_s\": %.9f,\n"
            "      \"t_call_fire_s\": %.9f,\n"
            "      \"t_call_ack_s\": %.9f\n"
            "    },\n"
            "    {\n"
            "      \"label\": \"slow_set_frequency\",\n"
            "      \"method\": \"bladerf_set_frequency (full PLL recalibration)\",\n"
            "      \"target_freq_hz\": %.6f,\n"
            "      \"marker_axis\": \"Q\",\n"
            "      \"marker_sample_idx\": %lu,\n"
            "      \"t_mark_s\": %.9f,\n"
            "      \"t_call_fire_s\": %.9f,\n"
            "      \"t_call_ack_s\": %.9f\n"
            "    }\n"
            "  ],\n"
            "  \"t_stop_s\": %.9f,\n"
            "  \"total_samples\": %lu,\n"
            "  \"iq_path\": \"%s\",\n"
            "  \"format\": \"cf32 (interleaved float32 I,Q little-endian)\"\n"
            "}\n",
            a.device_args,
            a.channel,
            a.sample_rate,
            a.start_freq_hz,
            a.target_freq_hz,
            slow_target_hz,
            a.slow_offset_hz,
            a.switch_ms,
            a.switch_ms_slow,
            a.gain_db,
            (double)a.mark_amp,
            a.mark_samples,
            a.total_ms,
            /* event 1 */
            a.target_freq_hz,
            (unsigned long)quick_mark_idx,
            t_quick_mark_s,
            t_quick_fire_s,
            t_quick_ack_s,
            /* event 2 */
            slow_target_hz,
            (unsigned long)slow_mark_idx,
            t_slow_mark_s,
            t_slow_fire_s,
            t_slow_ack_s,
            t_stop_s,
            (unsigned long)total_samples_written,
            a.iq_path);
        fclose(meta_fp);
        meta_fp = NULL;
        printf("Wrote metadata to %s\n", a.meta_path);
    }

cleanup:
    free(sc16_buf);
    free(cf32_buf);
    if (iq_fp)   fclose(iq_fp);
    if (meta_fp) fclose(meta_fp);
    if (dev)     bladerf_close(dev);
    return rc;
}
