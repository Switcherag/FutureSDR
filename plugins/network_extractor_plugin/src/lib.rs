//! Bridge block sitting after a PHY's decoder/MAC. Pulls "the data" out of
//! each decoded frame and emits a single-line ASCII record (Pmt::Blob) for
//! the downstream message-file-sink to log.
//!
//! Two modes selected via the config string:
//!
//! - `"zigbee"` — scans the MAC blob for the 17-byte control payload
//!   `step:u32_le | run:u16_le | tag:u8 ('Z'/'H') | wait_ms:u16_le | ts_us:u64_le`
//!   (the same payload the TX benchmark embeds) and emits
//!   `Z,<step>,<run>,<wait_ms>,<ts_us>,<rx_unix_us>`.
//!
//! - `"halow"` — parses an 802.11 Probe-Request frame and pulls out the
//!   SSID Information Element (Tag 0). Emits
//!   `H,<ssid>,<rx_unix_us>` and, if a control payload is also present in
//!   the frame body (vendor IE / payload area), appends `,<step>,<run>,<wait_ms>,<ts_us>`.
//!
//! Anything that doesn't parse cleanly produces a `?,<len>,<rx_unix_us>`
//! line so the file-sink still records the event.

use futuresdr::prelude::*;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Block)]
#[message_inputs(rx)]
#[message_outputs(out)]
#[null_kernel]
pub struct NetworkExtractor {
    phy: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ControlPayload {
    step: u32,
    run: Option<u16>,
    tag: u8,
    wait_ms: u16,
    ts_us: u64,
}

impl NetworkExtractor {
    pub fn new(phy: String) -> Self {
        Self { phy }
    }

    async fn rx(
        &mut self,
        io: &mut WorkIo,
        mio: &mut MessageOutputs,
        _meta: &mut BlockMeta,
        p: Pmt,
    ) -> Result<Pmt> {
        match p {
            Pmt::Blob(blob) => {
                let rx_us = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_micros() as u64)
                    .unwrap_or(0);
                let line = match self.phy.as_str() {
                    "halow" | "wlan_ah" | "wlan" => extract_halow(&blob, rx_us),
                    "auto" => extract_auto(&blob, rx_us),
                    _ => extract_zigbee(&blob, rx_us),
                };
                mio.post("out", Pmt::Blob(line.into_bytes())).await?;
            }
            Pmt::Finished => {
                io.finished = true;
            }
            _ => {}
        }
        Ok(Pmt::Ok)
    }
}

/// Search for the control payload anywhere in `blob`.
///
/// The current layout is
/// `step:u32_le | run:u16_le | tag:u8 | wait_ms:u16_le | ts_us:u64_le`.
/// Older captures used the 15-byte layout without `run`; keep a fallback so
/// existing logs remain parseable while the new transmitter rolls out.
fn find_control_payload(blob: &[u8]) -> Option<ControlPayload> {
    if blob.len() >= 17 {
        for i in 0..=(blob.len() - 17) {
            let step = u32::from_le_bytes([blob[i], blob[i + 1], blob[i + 2], blob[i + 3]]);
            if step > 600 {
                continue;
            }
            let run = u16::from_le_bytes([blob[i + 4], blob[i + 5]]);
            let tag = blob[i + 6];
            if tag != b'Z' && tag != b'H' {
                continue;
            }
            let wait_ms = u16::from_le_bytes([blob[i + 7], blob[i + 8]]);
            if wait_ms > 600 {
                continue;
            }
            let ts_us = u64::from_le_bytes([
                blob[i + 9],
                blob[i + 10],
                blob[i + 11],
                blob[i + 12],
                blob[i + 13],
                blob[i + 14],
                blob[i + 15],
                blob[i + 16],
            ]);
            return Some(ControlPayload {
                step,
                run: Some(run),
                tag,
                wait_ms,
                ts_us,
            });
        }
    }

    if blob.len() < 15 {
        return None;
    }

    for i in 0..=(blob.len() - 15) {
        let step = u32::from_le_bytes([blob[i], blob[i + 1], blob[i + 2], blob[i + 3]]);
        if step > 600 {
            continue;
        }
        let tag = blob[i + 4];
        if tag != b'Z' && tag != b'H' {
            continue;
        }
        let wait_ms = u16::from_le_bytes([blob[i + 5], blob[i + 6]]);
        if wait_ms > 600 {
            continue;
        }
        let ts_us = u64::from_le_bytes([
            blob[i + 7],
            blob[i + 8],
            blob[i + 9],
            blob[i + 10],
            blob[i + 11],
            blob[i + 12],
            blob[i + 13],
            blob[i + 14],
        ]);
        return Some(ControlPayload {
            step,
            run: None,
            tag,
            wait_ms,
            ts_us,
        });
    }
    None
}

fn extract_zigbee(blob: &[u8], rx_us: u64) -> String {
    match find_control_payload(blob) {
        Some(ControlPayload {
            step,
            run: Some(run),
            wait_ms,
            ts_us,
            ..
        }) => format!("Z,{step},{run},{wait_ms},{ts_us},{rx_us}\n"),
        Some(ControlPayload {
            step,
            wait_ms,
            ts_us,
            ..
        }) => format!("Z,{step},{wait_ms},{ts_us},{rx_us}\n"),
        None => format!("?,Z,{},{rx_us}\n", blob.len()),
    }
}

/// Walk an 802.11 management frame body and return the SSID IE value as a
/// UTF-8-lossy string. The walker is forgiving: it skips a leading MAC
/// header by trying common offsets, and on failure returns an empty SSID.
fn extract_ssid(blob: &[u8]) -> Option<String> {
    // Probe-Request layout (after PHY framing): 24-byte MAC header (no addr4,
    // no QoS) followed by IEs. SSID is Tag 0, Length L, Value (L bytes).
    // We try a few candidate IE-region start offsets to be robust against
    // RFta prefix / addr4 / HT-Control variants.
    for &start in &[24usize, 36, 12 + 24, 12 + 36, 0] {
        if start >= blob.len() {
            continue;
        }
        let ies = &blob[start..];
        let mut i = 0;
        while i + 2 <= ies.len() {
            let tag = ies[i];
            let len = ies[i + 1] as usize;
            if i + 2 + len > ies.len() {
                break;
            }
            if tag == 0 {
                // SSID IE
                let raw = &ies[i + 2..i + 2 + len];
                let s = String::from_utf8_lossy(raw).to_string();
                // Sanity: SSID is 0..=32 bytes
                if len <= 32 {
                    return Some(s);
                }
            }
            i += 2 + len;
        }
    }
    None
}

/// Dispatch by the tag byte found inside the control payload. If no
/// payload signature is present, fall back to a `?` line so the file
/// still records the event.
fn extract_auto(blob: &[u8], rx_us: u64) -> String {
    match find_control_payload(blob) {
        Some(ControlPayload {
            step,
            run,
            tag: b'H',
            wait_ms,
            ts_us,
        }) => {
            let ssid = extract_ssid(blob).unwrap_or_default();
            let ssid_safe: String = ssid
                .chars()
                .map(|c| if c == ',' || c == '\n' || (c as u32) < 0x20 { '.' } else { c })
                .collect();
            match run {
                Some(run) => format!("H,{ssid_safe},{step},{run},{wait_ms},{ts_us},{rx_us}\n"),
                None => format!("H,{ssid_safe},{step},{wait_ms},{ts_us},{rx_us}\n"),
            }
        }
        Some(ControlPayload {
            step,
            run: Some(run),
            wait_ms,
            ts_us,
            ..
        }) => format!("Z,{step},{run},{wait_ms},{ts_us},{rx_us}\n"),
        Some(ControlPayload {
            step,
            wait_ms,
            ts_us,
            ..
        }) => format!("Z,{step},{wait_ms},{ts_us},{rx_us}\n"),
        None => format!("?,{},{rx_us}\n", blob.len()),
    }
}

fn extract_halow(blob: &[u8], rx_us: u64) -> String {
    let ssid = extract_ssid(blob).unwrap_or_else(|| String::from(""));
    let ssid_safe: String = ssid
        .chars()
        .map(|c| {
            if c == ',' || c == '\n' || (c as u32) < 0x20 {
                '.'
            } else {
                c
            }
        })
        .collect();
    match find_control_payload(blob) {
        Some(ControlPayload {
            step,
            run: Some(run),
            wait_ms,
            ts_us,
            ..
        }) => format!("H,{ssid_safe},{step},{run},{wait_ms},{ts_us},{rx_us}\n"),
        Some(ControlPayload {
            step,
            wait_ms,
            ts_us,
            ..
        }) => format!("H,{ssid_safe},{step},{wait_ms},{ts_us},{rx_us}\n"),
        None => format!("H,{ssid_safe},,,,{rx_us}\n"),
    }
}

#[cfg(test)]
mod tests {
    use super::{ControlPayload, find_control_payload};

    #[test]
    fn parses_new_control_payload() {
        let mut blob = vec![0xaa, 0xbb, 0xcc];
        blob.extend_from_slice(&123u32.to_le_bytes());
        blob.extend_from_slice(&7u16.to_le_bytes());
        blob.push(b'H');
        blob.extend_from_slice(&42u16.to_le_bytes());
        blob.extend_from_slice(&999_888u64.to_le_bytes());
        blob.extend_from_slice(&[0xdd, 0xee]);

        assert_eq!(
            find_control_payload(&blob),
            Some(ControlPayload {
                step: 123,
                run: Some(7),
                tag: b'H',
                wait_ms: 42,
                ts_us: 999_888,
            })
        );
    }

    #[test]
    fn falls_back_to_legacy_control_payload() {
        let mut blob = vec![0xaa];
        blob.extend_from_slice(&321u32.to_le_bytes());
        blob.push(b'Z');
        blob.extend_from_slice(&55u16.to_le_bytes());
        blob.extend_from_slice(&777u64.to_le_bytes());

        assert_eq!(
            find_control_payload(&blob),
            Some(ControlPayload {
                step: 321,
                run: None,
                tag: b'Z',
                wait_ms: 55,
                ts_us: 777,
            })
        );
    }
}

plugin_api::export_plugin! {
    name: "NetworkExtractor",
    description: "Extract per-frame data (zigbee payload / halow SSID) into a CSV-ish line",
    config: String,
    create: |cfg, _id| {
        NetworkExtractor::new(cfg)
    }
}
