// SDR cross-flowgraph: SeifySource (permanent) + Zigbee/discard (swappable)
//
// FG0: SeifySource → [auto bridge c32] → FG1
// FG1: [auto bridge c32] → ZigbeeDemod → ZigbeeDecoder
//
// Usage:
//   cargo build -p sdr-cross-flowgraph-example \
//     -p seify_source_plugin -p zigbee_demod_plugin -p zigbee_decoder_plugin \
//     -p null_sink_plugin
//   cd examples/sdr_cross_flowgraph && ../../target/debug/sdr_cross_fg
//
// UDP control (port 7879):
//   echo -n '-s flows/zigbee_rx.toml' | nc -u 127.0.0.1 7879
//   echo -n '-s flows/discard.toml'   | nc -u 127.0.0.1 7879
//   echo -n Q                         | nc -u 127.0.0.1 7879

use plugin_host::{FlowgraphController, default_plugin_dir};

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    futuresdr::runtime::init();
    FlowgraphController::builder(default_plugin_dir())
        .add_permanent("flows/fg0_sdr.toml")        // fg/0/ — SDR source
        .add_swappable("flows/wlan_rx.toml")          // fg/1/ — WLAN receiver
        .connect(0, "out", 1, "in")
        .udp_port(7879)
        .run()
}
