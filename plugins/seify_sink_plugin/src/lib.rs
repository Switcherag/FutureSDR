use futuresdr::blocks::seify::Builder;

// Config: (device_args, frequency_hz, sample_rate_hz, gain_db)
//
// - device_args: Device argument string (e.g. "driver=hackrf")
// - frequency_hz: Center frequency in Hz
// - sample_rate_hz: Sample rate in samples/sec
// - gain_db: TX gain in dB
//
// Pass as: Box::new(("driver=hackrf".to_string(), 100e6_f64, 2.4e6_f64, 40.0_f64))
plugin_api::export_plugin! {
    name: "SeifySink",
    description: "SDR radio sink via seify (supports SoapySDR, HackRF, ...)",
    config: (String, f64, f64, f64),
    create: |cfg, _id| {
        let (args, freq, sample_rate, gain) = cfg;
        Builder::new(args.as_str())
            .expect("failed to open seify device")
            .frequency(freq)
            .sample_rate(sample_rate)
            .gain(gain)
            .build_sink()
            .expect("failed to build seify sink")
    }
}
