//! Universal MAC — placeholder bypass.
//!
//! Sits in the permanent tail between the inter-FG bridge (which delivers
//! RFtap-encapsulated frames from whichever PHY is currently active) and
//! the downstream consumers (UDP RFtap egress, TAP NIC, network extractor).
//! Today it just forwards `Pmt::Blob` unchanged; this is the seam where a
//! future "give NIC a clean, normalized frame" layer plugs in (de-dup,
//! ack/retry handling, MAC-level addressing, …).

use futuresdr::prelude::*;

#[derive(Block)]
#[message_inputs(r#in)]
#[message_outputs(out)]
#[null_kernel]
pub struct UniversalMac;

impl UniversalMac {
    pub fn new() -> Self {
        Self
    }

    async fn r#in(
        &mut self,
        io: &mut WorkIo,
        mio: &mut MessageOutputs,
        _meta: &mut BlockMeta,
        p: Pmt,
    ) -> Result<Pmt> {
        match p {
            Pmt::Finished => {
                io.finished = true;
            }
            p => {
                mio.post("out", p).await?;
            }
        }
        Ok(Pmt::Ok)
    }
}

impl Default for UniversalMac {
    fn default() -> Self {
        Self::new()
    }
}

plugin_api::export_plugin! {
    name: "UniversalMac",
    description: "Universal MAC (bypass for now) — passes Pmt::Blob through.",
    config: (),
    create: |_cfg, _id| {
        UniversalMac::new()
    }
}
