cargo clean --release 2>&1 | tail -3 && cargo build --release -p real-device-swap-example \
  -p seify_source_plugin -p fir_resampler_plugin \
  -p zigbee_demod_plugin -p clock_recovery_mm_plugin -p zigbee_decoder_plugin -p zigbee_mac_plugin -p null_sink_plugin \
  -p blob_to_udp_plugin \
  -p wlan_ah_v2_stf_detector_plugin -p wlan_ah_v2_cfo_corrector_plugin -p wlan_ah_v2_sto_corrector_plugin \
  -p wlan_ah_v2_channel_estimator_plugin -p wlan_ah_v2_sig_decoder_plugin -p wlan_ah_v2_data_demod_plugin \
  -p wlan_ah_decoder_plugin \
  -p network_extractor_plugin -p message_file_sink_plugin \
  -p blob_to_lp_stream_plugin -p lp_stream_to_blob_plugin



