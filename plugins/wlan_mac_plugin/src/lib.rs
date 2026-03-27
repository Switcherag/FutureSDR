#![allow(clippy::needless_range_loop)]

use futuresdr::prelude::*;

const MAX_PAYLOAD_SIZE: usize = 1500;
const MAX_PSDU_SIZE: usize = MAX_PAYLOAD_SIZE + 28;

#[derive(Clone, Copy, Debug)]
#[allow(non_camel_case_types)]
pub enum Mcs {
    Bpsk_1_2,
    Bpsk_3_4,
    Qpsk_1_2,
    Qpsk_3_4,
    Qam16_1_2,
    Qam16_3_4,
    Qam64_2_3,
    Qam64_3_4,
}

#[derive(Block)]
#[message_inputs(tx)]
#[message_outputs(tx)]
pub struct Mac {
    current_frame: [u8; MAX_PSDU_SIZE],
    sequence_number: u16,
}

impl Mac {
    pub fn new(src_mac: [u8; 6], dst_mac: [u8; 6], bss_mac: [u8; 6]) -> Self {
        let mut current_frame = [0; MAX_PSDU_SIZE];

        // frame control
        current_frame[0..2].copy_from_slice(&0x0008u16.to_le_bytes());
        // duration
        current_frame[2..4].copy_from_slice(&0x0000u16.to_le_bytes());
        // mac addresses
        current_frame[4..10].copy_from_slice(&src_mac);
        current_frame[10..16].copy_from_slice(&dst_mac);
        current_frame[16..22].copy_from_slice(&bss_mac);

        Mac {
            current_frame,
            sequence_number: 0,
        }
    }

    async fn tx(
        &mut self,
        io: &mut WorkIo,
        mio: &mut MessageOutputs,
        _meta: &mut BlockMeta,
        p: Pmt,
    ) -> Result<Pmt> {
        match p {
            Pmt::Blob(data) => {
                if data.len() > MAX_PAYLOAD_SIZE {
                    warn!(
                        "WLAN Mac: TX frame too large ({}, max {}). Dropping.",
                        data.len(),
                        MAX_PAYLOAD_SIZE
                    );
                } else {
                    let len = self.generate_mac_data_frame(&data);
                    debug!("mac frame {:?}", &self.current_frame[0..len]);
                    let mut vec = vec![0; len];
                    vec.copy_from_slice(&self.current_frame[0..len]);
                    mio.post("tx", Pmt::Any(Box::new((vec, None as Option<Mcs>))))
                        .await?;
                }
            }
            Pmt::Any(a) => {
                if let Some((data, mcs)) = a.downcast_ref::<(Vec<u8>, Mcs)>() {
                    if data.len() > MAX_PAYLOAD_SIZE {
                        warn!(
                            "WLAN Mac: TX frame too large ({}, max {}). Dropping.",
                            data.len(),
                            MAX_PAYLOAD_SIZE
                        );
                    } else {
                        let len = self.generate_mac_data_frame(data);
                        debug!("mac frame {:?}", &self.current_frame[0..len]);
                        let mut vec = vec![0; len];
                        vec.copy_from_slice(&self.current_frame[0..len]);
                        mio.post("tx", Pmt::Any(Box::new((vec, Some(*mcs)))))
                            .await?;
                    }
                }
            }
            Pmt::Finished => {
                io.finished = true;
            }
            x => {
                warn!("WLAN Mac: received wrong PMT type in TX callback. {:?}", x);
            }
        }
        Ok(Pmt::Null)
    }

    fn generate_mac_data_frame(&mut self, data: &[u8]) -> usize {
        self.current_frame[22..24].copy_from_slice(&(self.sequence_number << 4).to_le_bytes());
        self.sequence_number = (self.sequence_number + 1) % (1 << 12);

        let len = data.len() + 24;

        unsafe {
            std::ptr::copy_nonoverlapping(
                data.as_ptr(),
                self.current_frame.as_mut_ptr().add(24),
                data.len(),
            );
        }

        let crc = crc32fast::hash(&self.current_frame[0..len]);
        self.current_frame[len..len + 4].copy_from_slice(&crc.to_le_bytes());

        len + 4
    }
}

impl Kernel for Mac {}

plugin_api::export_plugin! {
    name: "WlanMac",
    description: "WLAN 802.11 MAC layer with CRC32",
    config: ([u8; 6], [u8; 6], [u8; 6]),
    create: |cfg, _id| {
        Mac::new(cfg.0, cfg.1, cfg.2)
    }
}
