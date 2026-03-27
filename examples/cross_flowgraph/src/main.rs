// Cross-flowgraph example with UDP-controlled switching.
//
// Uses FlowgraphController from plugin_api to hot-swap FG1 from TOML files.
//
// Usage:
//   cargo build -p cross-flowgraph-example -p null_source_plugin -p throttle_plugin \
//     -p selector_1_2_plugin -p null_sink_plugin -p bridge_sink_plugin \
//     -p bridge_source_plugin -p print_sink_plugin -p copy_plugin
//   cd examples/cross_flowgraph && ../../target/debug/cross_fg [initial.toml]
//
// UDP control (port 7878):
//   echo -n '-s flows/flow_a.toml' | nc -u 127.0.0.1 7878   → swap FG1
//   echo -n Q | nc -u 127.0.0.1 7878                        → quit

use anyhow::Result;
use futuresdr::blocks::SelectorDropPolicy;
use futuresdr::runtime::Runtime;
use plugin_api::{FlowgraphController, default_plugin_dir};
use std::net::UdpSocket;
use std::time::Duration;

fn main() -> Result<()> {
    futuresdr::runtime::init();

    let dir = default_plugin_dir();

    // ── Build FG0 (permanent) using the controller's registry ────
    // We create a temporary flowgraph first to get the selector_id,
    // then build the controller around it.
    let mut fg0 = futuresdr::runtime::Flowgraph::new();

    // Pre-load FG0 plugins via a temporary registry
    let mut tmp_reg = plugin_api::PluginRegistry::new(dir.clone());
    println!("Loading FG0 plugins from {dir} ...");
    for name in &[
        "null_source_plugin",
        "throttle_plugin",
        "selector_1_2_plugin",
        "null_sink_plugin",
        "bridge_sink_plugin",
    ] {
        tmp_reg.ensure_loaded(name);
    }

    let src_id = fg0.add_block_dyn(tmp_reg.get("null_source_plugin").prepare(Box::new(())));
    let thr_id = fg0.add_block_dyn(tmp_reg.get("throttle_plugin").prepare(Box::new(1.0_f64)));
    let sel_id = fg0.add_block_dyn(
        tmp_reg.get("selector_1_2_plugin").prepare(Box::new(SelectorDropPolicy::DropAll)),
    );
    let snk_id = fg0.add_block_dyn(tmp_reg.get("null_sink_plugin").prepare(Box::new(())));

    // Create the controller (owns the shared buffer and selector_id)
    let mut ctrl = FlowgraphController::new(dir, sel_id);
    // Transfer pre-loaded plugins into the controller's registry
    ctrl.registry = tmp_reg;

    let bsink_id = fg0.add_block_dyn(
        ctrl.registry.get("bridge_sink_plugin").prepare(Box::new(ctrl.shared_buf())),
    );

    fg0.connect_dyn(src_id, "output", thr_id, "input")?;
    fg0.connect_dyn(thr_id, "output", sel_id, "inputs[0]")?;
    fg0.connect_dyn(sel_id, "outputs[0]", snk_id, "input")?;
    fg0.connect_dyn(sel_id, "outputs[1]", bsink_id, "input")?;
    println!("FG0 plugins loaded.\n");

    // ── Start FG0 ────────────────────────────────────────────────
    let rt = Runtime::new();
    let rt_handle = rt.handle();
    println!("Starting Flowgraph 0 (permanent) ...");
    let (fg0_task, fg0_handle) = rt.start_sync(fg0)?;
    drop(fg0_task);
    println!("Flowgraph 0 running.\n");

    // ── UDP socket ───────────────────────────────────────────────
    let socket = UdpSocket::bind("0.0.0.0:7878")?;
    socket.set_read_timeout(Some(Duration::from_millis(500)))?;

    println!("Listening for UDP commands on port 7878");
    println!("  echo -n '-s flows/flow_a.toml' | nc -u 127.0.0.1 7878   → swap FG1");
    println!("  echo -n Q | nc -u 127.0.0.1 7878                        → quit\n");

    let initial_toml = std::env::args().nth(1).unwrap_or_else(|| "flows/flow_a.toml".into());

    // ── Async control loop ───────────────────────────────────────
    rt.block_on(async move {
        let mut fg0_handle = fg0_handle;

        // Start initial FG1 from TOML
        println!("Loading initial flowgraph from '{initial_toml}' ...");
        let mut fg1_handle = ctrl
            .start_initial(&initial_toml, &mut fg0_handle, &rt_handle)
            .await
            .expect("failed to load initial flowgraph");
        println!("Flowgraph 1 running.\n");

        let mut udp_buf = [0u8; 512];
        loop {
            futuresdr::async_io::Timer::after(Duration::from_millis(100)).await;

            match socket.recv_from(&mut udp_buf) {
                Ok((n, addr)) => {
                    let cmd = std::str::from_utf8(&udp_buf[..n])
                        .unwrap_or("")
                        .trim();
                    println!("UDP from {addr}: \"{cmd}\"");

                    if cmd.eq_ignore_ascii_case("q") {
                        println!("Shutting down ...");
                        ctrl.shutdown(&mut fg0_handle, &mut fg1_handle)
                            .await
                            .unwrap();
                        break;
                    } else if let Some(path) = cmd.strip_prefix("-s ").or_else(|| cmd.strip_prefix("-S ")) {
                        let path = path.trim();
                        println!("Swapping to flowgraph '{path}' ...");

                        match ctrl.swap(path, &mut fg0_handle, &mut fg1_handle, &rt_handle).await {
                            Ok(()) => println!("[{path}] active.\n"),
                            Err(e) => eprintln!("ERROR: {e}\n"),
                        }
                    } else {
                        println!("Unknown command. Use: -s <path.toml> | Q");
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => eprintln!("UDP error: {e}"),
            }
        }
    });

    println!("Done.");
    Ok(())
}
