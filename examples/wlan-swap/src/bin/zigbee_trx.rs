use anyhow::Result;
use clap::Parser;
use futuresdr::async_io::Timer;
use futuresdr::async_io::block_on;
use futuresdr::blocks::Apply;
use futuresdr::blocks::WebsocketPmtSink;
use futuresdr::prelude::*;
use std::time::Duration;

use wlan::zigbee::ClockRecoveryMm;
use wlan::zigbee::Decoder;
use wlan::zigbee::IqDelay;
use wlan::zigbee::Mac;
use wlan::zigbee::modulator;

#[derive(Parser, Debug)]
#[clap(version)]
struct Args {
    
}

fn main() -> Result<()> {
    let args = Args::parse();
    println!("Configuration: {args:?}");

    let mut fg = Flowgraph::new();

    // ========================================
    // TRANSMITTER
    // ========================================
    let mac: Mac = Mac::new();
    let mac = fg.add_block(mac);
    let modulator = modulator(&mut fg);
    let iq_delay: IqDelay = IqDelay::new();
    let iq_delay = fg.add_block(iq_delay);

    fg.connect_dyn(&mac, "output", modulator, "input")?;
    fg.connect_dyn(modulator, "output", &iq_delay, "input")?;

    // ========================================
    // Receiver (perfect loopback - no radio)
    // ========================================
    let mut last: Complex32 = Complex32::new(0.0, 0.0);
    let mut iir: f32 = 0.0;
    let alpha = 0.00016;
    let avg = fg.add_block(Apply::<_, _, _>::new(move |i: &Complex32| -> f32 {
        let phase = (last.conj() * i).arg();
        last = *i;
        iir = (1.0 - alpha) * iir + alpha * phase;
        phase - iir
    }));

    let omega = 2.0;
    let gain_omega = 0.000225;
    let mu = 0.5;
    let gain_mu = 0.03;
    let omega_relative_limit = 0.0002;
    let mm: ClockRecoveryMm =
        ClockRecoveryMm::new(omega, gain_omega, mu, gain_mu, omega_relative_limit);
    let mm = fg.add_block(mm);

    let decoder: Decoder = Decoder::new(12);  // Increased threshold for software loopback
    let decoder = fg.add_block(decoder);

    // Perfect loopback: iq_delay -> receiver chain
    fg.connect_dyn(&iq_delay, "output", &avg, "input")?;
    fg.connect_dyn(&avg, "output", &mm, "input")?;
    fg.connect_dyn(&mm, "output", &decoder, "input")?;

    // Clock recovery symbols on port 9002
    let symbol_sink_mm = WebsocketPmtSink::new(9002);
    connect!(fg, mm.symbols | symbol_sink_mm);
    

    
    let mac = mac.into();

    let rt = Runtime::new();
    let (fg, mut handle) = rt.start_sync(fg)?;

    // send a message every 0.8 seconds
    let mut seq = 0u64;
    rt.spawn_background(async move {
        loop {
            Timer::after(Duration::from_secs_f32(0.06)).await;
            handle
                .call(
                    mac,
                    "tx",
                    Pmt::Blob(format!("FutureSDR {seq}").as_bytes().to_vec()),
                )
                .await
                .unwrap();
            seq += 1;
        }
    });

    block_on(fg)?;

    Ok(())
}
