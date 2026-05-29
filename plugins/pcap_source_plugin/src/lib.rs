//! Replay SDR-NIC pcap captures as a stream of `Pmt::Blob` frame messages,
//! interleaving multiple captures one frame at a time.
//!
//! Config is a comma-separated list of pcap paths
//! (`config_type = "String"`, e.g. `"a.pcap,b.pcap"`). Each file is read at
//! construction and reduced to its RFtap-encapsulated SDR frames: the captures
//! are TAP recordings, so the 14-byte Ethernet/MAC header is stripped, leaving
//! the bare RFtap frame (starting with the `"RFta"` magic) — the same shape the
//! real PHY decoders emit on their `rftap` ports, and the form a downstream
//! UDP sink can hand straight to Wireshark's RFtap dissector. The block then
//! emits one frame every [`PACE`], round-robin across the captures —
//! `a[0], b[0], a[1], b[1], …` — wrapping each capture independently when it
//! runs out. With a single path it is just a paced replay of that one file.
//!
//! Pair with `blob_to_lp_stream_plugin` to ship the blobs across an
//! inter-flowgraph bridge into a TAP-NIC tail.

use futuresdr::async_io::Timer;
use futuresdr::prelude::*;
use std::time::Duration;

/// Gap between emitted frames — one frame per second.
const PACE: Duration = Duration::from_secs(1);

struct Capture {
    name: String,
    frames: Vec<Vec<u8>>,
    cursor: usize,
}

#[derive(Block)]
#[message_outputs(out)]
pub struct PcapSource {
    captures: Vec<Capture>,
    next: usize,
}

impl PcapSource {
    pub fn new(cfg: String) -> Self {
        let mut captures = Vec::new();
        for path in cfg.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let frames = match std::fs::read(path) {
                Ok(data) => parse_pcap_88b5(&data),
                Err(e) => {
                    warn!("pcap_source: cannot read {path:?}: {e}");
                    Vec::new()
                }
            };
            let name = path.rsplit('/').next().unwrap_or(path).to_string();
            if frames.is_empty() {
                warn!("pcap_source: no 0x88B5 frames in {path:?} — skipping");
            } else {
                info!("pcap_source: loaded {} frame(s) from {path}", frames.len());
            }
            captures.push(Capture {
                name,
                frames,
                cursor: 0,
            });
        }
        Self { captures, next: 0 }
    }

    async fn sleep(dur: Duration) {
        Timer::after(dur).await;
    }
}

impl Kernel for PcapSource {
    async fn work(
        &mut self,
        io: &mut WorkIo,
        mio: &mut MessageOutputs,
        _meta: &mut BlockMeta,
    ) -> Result<()> {
        let n = self.captures.len();
        if n == 0 || self.captures.iter().all(|c| c.frames.is_empty()) {
            io.finished = true;
            return Ok(());
        }

        // Round-robin to the next capture that still has frames, emit exactly
        // one frame, advance that capture's own cursor (wrapping).
        for _ in 0..n {
            let i = self.next;
            self.next = (self.next + 1) % n;
            let cap = &mut self.captures[i];
            if cap.frames.is_empty() {
                continue;
            }
            let frame = cap.frames[cap.cursor].clone();
            cap.cursor = (cap.cursor + 1) % cap.frames.len();
            debug!("pcap_source: -> {} frame ({} bytes)", cap.name, frame.len());
            mio.post("out", Pmt::Blob(frame)).await?;
            break;
        }

        io.block_on(PcapSource::sleep(PACE));
        Ok(())
    }
}

/// Parse a classic-format pcap and return the RFtap payload of every Ethernet
/// frame whose ethertype is `0x88B5` (the RFtap-encapsulated SDR frames written
/// by `tap_nic_plugin`). The 14-byte Ethernet/MAC header is stripped, so each
/// returned blob starts with the `"RFta"` magic. Kernel housekeeping
/// (IPv6 MLD, etc.) is skipped.
fn parse_pcap_88b5(data: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    if data.len() < 24 {
        return out;
    }
    // Magic selects byte order (and µs vs ns timestamps, which we ignore).
    let le = match data[0..4] {
        [0xd4, 0xc3, 0xb2, 0xa1] | [0x4d, 0x3c, 0xb2, 0xa1] => true,
        [0xa1, 0xb2, 0xc3, 0xd4] | [0xa1, 0xb2, 0x3c, 0x4d] => false,
        _ => {
            warn!("pcap_source: unrecognized pcap magic — not a classic pcap");
            return out;
        }
    };
    let rd_u32 = |b: &[u8]| -> u32 {
        let a = [b[0], b[1], b[2], b[3]];
        if le {
            u32::from_le_bytes(a)
        } else {
            u32::from_be_bytes(a)
        }
    };

    // 24-byte global header, then 16-byte record headers + payloads.
    let mut pos = 24usize;
    while pos + 16 <= data.len() {
        let incl_len = rd_u32(&data[pos + 8..pos + 12]) as usize;
        pos += 16;
        if pos + incl_len > data.len() {
            break;
        }
        let frame = &data[pos..pos + incl_len];
        if frame.len() > 14 && frame[12] == 0x88 && frame[13] == 0xb5 {
            // Strip the 14-byte Ethernet header → bare RFtap frame.
            out.push(frame[14..].to_vec());
        }
        pos += incl_len;
    }
    out
}

plugin_api::export_plugin! {
    name: "PcapSource",
    description: "Interleave 0x88B5 Ethernet frames from one or more pcaps as periodic Pmt::Blob messages",
    config: String,
    create: |cfg, _id| {
        PcapSource::new(cfg)
    }
}

#[cfg(test)]
mod tests {
    use super::parse_pcap_88b5;

    fn record(payload: &[u8]) -> Vec<u8> {
        let mut r = Vec::new();
        r.extend_from_slice(&0u32.to_le_bytes()); // ts_sec
        r.extend_from_slice(&0u32.to_le_bytes()); // ts_usec
        r.extend_from_slice(&(payload.len() as u32).to_le_bytes()); // incl_len
        r.extend_from_slice(&(payload.len() as u32).to_le_bytes()); // orig_len
        r.extend_from_slice(payload);
        r
    }

    fn eth(ethertype: [u8; 2], body: &[u8]) -> Vec<u8> {
        let mut f = vec![0u8; 12];
        f.extend_from_slice(&ethertype);
        f.extend_from_slice(body);
        f
    }

    #[test]
    fn keeps_only_88b5_frames() {
        let mut data = Vec::new();
        data.extend_from_slice(&[0xd4, 0xc3, 0xb2, 0xa1]); // magic (LE, µs)
        data.extend_from_slice(&[0u8; 20]); // rest of global header
        data.extend(record(&eth([0x88, 0xb5], b"sdr-frame-1")));
        data.extend(record(&eth([0x86, 0xdd], b"ipv6-noise")));
        data.extend(record(&eth([0x88, 0xb5], b"sdr-frame-2")));

        let frames = parse_pcap_88b5(&data);
        assert_eq!(frames.len(), 2);
        // Ethernet header stripped: blob is the payload after the ethertype.
        assert_eq!(frames[0], b"sdr-frame-1");
        assert_eq!(frames[1], b"sdr-frame-2");
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_pcap_88b5(b"not a pcap file").is_empty());
        assert!(parse_pcap_88b5(&[]).is_empty());
    }
}
