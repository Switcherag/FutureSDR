// NIC_sim — replay recorded SDR-NIC pcaps onto a Linux TAP interface,
// interleaving two PHYs one frame at a time.
//
// Instead of a live SDR receiving frames, the replay source reads both
// captures and emits their 0x88B5 RFtap frames round-robin — one HaLow frame,
// then one Zigbee frame, then the next HaLow frame, … — one per second (see
// flows/replay.toml + pcap_source_plugin). The permanent `nic_tail` lands each
// frame back on the sdrtap0 TAP device (and mirrors it to UDP/55555).
//
// TAP setup (needs CAP_NET_ADMIN — do this once, then run unprivileged):
//   sudo ip tuntap add dev sdrtap0 mode tap user "$USER"
//   sudo ip link set sdrtap0 address de:ad:be:ef:00:01
//   sudo ip link set sdrtap0 up
//
// Build + run:
//   examples/NIC_sim/build.sh
//   cd examples/NIC_sim
//   ../../target/release/nic_sim
//
// Observe:
//   tcpdump -i sdrtap0           # frames on the TAP NIC
//   nc -u -l 55555               # UDP mirror (no TAP privileges needed)

use futuresdr::futures::StreamExt;
use futuresdr::runtime::Pmt;
use plugin_host::{FlowgraphController, default_plugin_dir};

/// Label a replayed frame by the RFtap DLT byte. Frames are bare RFtap now
/// ("RFta" magic + 2x u16 + u32 DLT), so the DLT low byte is at offset 8.
/// 195 = 802.15.4 (Zigbee), 105 = 802.11 (HaLow).
fn phy_label(frame: &[u8]) -> &'static str {
    match frame.get(8) {
        Some(0xc3) => "Zigbee",
        Some(0x69) => "HaLow",
        _ => "?",
    }
}

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    futuresdr::runtime::init();

    println!("=== NIC_sim — interleaved pcap replay, one frame per second ===");
    println!("Source: flows/replay.toml   TAP iface: sdrtap0   UDP mirror: 127.0.0.1:55555\n");

    let (builder, mut tap_rx) = FlowgraphController::builder(default_plugin_dir())
        .add_permanent("flows/nic_tail.toml")
        .add_swappable("flows/replay.toml")
        .tap_channel(256);

    builder.run_with(move |mut ctrl, rt_handle, entries| async move {
        // Permanent tail first (owns the TAP fd), then the replay source.
        for &(idx, ref path, permanent) in &entries {
            if permanent {
                println!("Starting tail fg/{idx}/ from '{path}' ...");
                ctrl.start_permanent(idx, path, &rt_handle).await?;
            }
        }
        ctrl.activate_selectors().await?;
        for &(idx, ref path, permanent) in &entries {
            if !permanent {
                println!("Starting source fg/{idx}/ from '{path}' ...");
                ctrl.start_swappable(idx, path, &rt_handle).await?;
            }
        }

        println!("\nReplaying frame-by-frame. Observe with:  tcpdump -i sdrtap0   (or  nc -u -l 55555)");
        println!("Press Ctrl-C to quit.\n");

        // One frame arrives per second, alternating PHY; log each as it lands.
        let mut n = 0u64;
        while let Some((_tap, pmt)) = tap_rx.next().await {
            if let Pmt::Blob(frame) = pmt {
                n += 1;
                println!("frame #{n:<4} -> {:<6} ({} bytes)", phy_label(&frame), frame.len());
            }
        }
        Ok(())
    })
}
