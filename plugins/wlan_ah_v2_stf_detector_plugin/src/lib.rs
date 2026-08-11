use futuresdr::prelude::*;
use num_complex::Complex32;
use wlan_ah::v2::StfDetector;

// Config: the largest PSDU (bytes) this receiver expects to see.
//
// The detector buffers one whole frame length past a detection candidate
// before it will emit, because the segment handed downstream is a fixed
// worst-case slice — the true length is only known once SIG is decoded. So
// this doubles as the block's cold-start latency: a freshly built flowgraph
// decodes nothing until it has accumulated that much signal.
//
//   0 (or no config) → the PHY maximum, 1500 B ≈ 76,448 samples ≈ 19 ms @ 4 MSps
//   34               → 3,008 samples ≈ 0.75 ms @ 4 MSps
//
// Sizing it to the traffic is what makes per-frame flowgraph swapping viable:
// with the default, every swap costs ~19 ms of re-acquisition. Frames longer
// than the configured size are not captured.
plugin_api::export_plugin! {
    name: "WlanAhV2StfDetector",
    description: "802.11ah v2 STF detector — config: max PSDU in bytes (0 = PHY max)",
    config: usize,
    create: |cfg, _id| {
        if cfg == 0 {
            StfDetector::<DefaultCpuReader<Complex32>>::new()
        } else {
            StfDetector::<DefaultCpuReader<Complex32>>::with_max_psdu(cfg)
        }
    }
}
