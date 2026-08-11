//! Dual-PHY listen-only swap.
//!
//! Minimal midpoint between `real_device_swap` and `ack`: one SDR head listens
//! on one PHY, and each decoded frame triggers a swap of the listener to the
//! other PHY.
//!
//! Run (from this directory, after `./build.sh`):
//!   cd examples/dual_phy_handshake
//!   ../../target/release/listen

use futuresdr::futures::StreamExt;
use futuresdr::runtime::Pmt;
use plugin_host::{FlowgraphController, default_plugin_dir};

const HEAD_FLOW: &str = "flows/sdr_head_listen.toml";
const LISTEN: [&str; 2] = ["flows/halow_listen2.toml", "flows/halow_listen1.toml"];
const NAME: [&str; 2] = ["802.15.4", "802.11ah"];

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    futuresdr::runtime::init();

    let (builder, mut tap_rx) = FlowgraphController::builder(default_plugin_dir())
        .add_head(HEAD_FLOW)
        .add_swappable(LISTEN[0])
        .tap_channel(128);

    builder.run_with(move |mut ctrl, rt_handle, entries| async move {
        // Start the permanent head first, then activate selectors, then start
        // the first listener flowgraph.
        for &(idx, ref path, perm) in &entries {
            if perm {
                ctrl.start_permanent(idx, path, &rt_handle).await?;
            }
        }
        ctrl.activate_selectors().await?;
        for &(idx, ref path, perm) in &entries {
            if !perm {
                ctrl.start_swappable(idx, path, &rt_handle).await?;
            }
        }

        let listen_idx = entries
            .iter()
            .find_map(|&(i, _, p)| (!p).then_some(i))
            .expect("one swappable listen flowgraph");

        let mut cur = 0usize;
        println!(
            "Listening on {} ({}) — swap on each received frame. Ctrl-C to quit.\n",
            NAME[cur], LISTEN[cur]
        );

        while let Some((tap_name, pmt)) = tap_rx.next().await {
            let n = match &pmt {
                Pmt::Blob(b) => b.len(),
                _ => 0,
            };
            let other = cur ^ 1;

            println!(
                "[rx {tap_name}] frame ({n} B) on {} -> swapping listener to {}",
                NAME[cur], NAME[other]
            );

            ctrl.swap(listen_idx, LISTEN[other], &rt_handle).await?;
            cur = other;

            println!("[swap] now listening on {} ({})", NAME[cur], LISTEN[cur]);

            // Drop frames that queued while swap was in progress.
            while tap_rx.try_recv().is_ok() {}
        }

        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })
}
