use futuresdr::prelude::*;

use crate::FrameParam;
use crate::MAX_ENCODED_BITS;
use crate::MAX_PSDU_SIZE;
use crate::MAX_SYM;
use crate::Mcs;
use crate::N_DATA_SC;
use crate::ViterbiDecoder;
use crate::crc8;

#[derive(Block)]
#[message_outputs(rx_frames, rftap)]
pub struct Decoder<I = DefaultCpuReader<u8>>
where
    I: CpuBufferReader<Item = u8>,
{
    #[input]
    input: I,
    frame_complete: bool,
    frame_param: FrameParam,
    decoder: ViterbiDecoder,
    copied: usize,
    rx_symbols: [u8; N_DATA_SC * MAX_SYM],
    rx_bits: [u8; MAX_ENCODED_BITS],
    deinterleaved_bits: [u8; MAX_ENCODED_BITS],
    decoded_bits: [u8; MAX_ENCODED_BITS],
    out_bytes: [u8; MAX_PSDU_SIZE + 2], // 2 for service field (1 byte) + alignment
}

impl<I> Decoder<I>
where
    I: CpuBufferReader<Item = u8>,
{
    pub fn new() -> Self {
        Self {
            input: I::default(),
            frame_complete: true,
            frame_param: FrameParam::new(Mcs::Bpsk_1_2, 0),
            decoder: ViterbiDecoder::new(),
            copied: 0,
            rx_symbols: [0; N_DATA_SC * MAX_SYM],
            rx_bits: [0; MAX_ENCODED_BITS],
            deinterleaved_bits: [0; MAX_ENCODED_BITS],
            decoded_bits: [0; MAX_ENCODED_BITS],
            out_bytes: [0; MAX_PSDU_SIZE + 2],
        }
    }

    /// 802.11ah deinterleaver (Table 21-17: Ncol=13, Nrow=4×Nbpscs).
    /// Uses the standard deinterleaver permutations (Section 21.3.10.7).
    fn deinterleave(&mut self) {
        let n_cbps = self.frame_param.mcs().n_cbps();
        let n_bpsc = self.frame_param.mcs().modulation().n_bpsc();
        let n_col: usize = 13;
        let s = std::cmp::max(n_bpsc / 2, 1);

        let mut first = vec![0usize; n_cbps];
        let mut second = vec![0usize; n_cbps];

        // Deinterleaver first permutation: j = s×floor(i/s) + (i + floor(Ncol×i/Ncbps)) mod s
        for j in 0..n_cbps {
            first[j] = s * (j / s) + ((j + (n_col * j / n_cbps)) % s);
        }
        // Deinterleaver second permutation: k = Ncol×j - (Ncbps-1)×floor(Ncol×j/Ncbps)
        for i in 0..n_cbps {
            second[i] = n_col * i - (n_cbps - 1) * (n_col * i / n_cbps);
        }

        for i in 0..self.frame_param.n_symbols() {
            for k in 0..n_cbps {
                self.deinterleaved_bits[i * n_cbps + second[first[k]]] =
                    self.rx_bits[i * n_cbps + k];
            }
        }
    }

    fn decode(&mut self) -> bool {
        let syms = self.frame_param.n_symbols();
        let bpsc = self.frame_param.mcs().modulation().n_bpsc();
        for i in 0..syms * N_DATA_SC {
            for k in 0..bpsc {
                self.rx_bits[i * bpsc + k] = u8::from((self.rx_symbols[i] & (1 << k)) > 0);
            }
        }

        self.deinterleave();
        self.decoder.decode(
            self.frame_param.clone(),
            &self.deinterleaved_bits,
            &mut self.decoded_bits,
        );
        self.descramble();

        // Always return true so frames are output even on CRC fail
        true
    }

    /// 802.11ah descrambler. SERVICE field is 8 bits (not 16 as in 802.11a/g).
    fn descramble(&mut self) {
        let decoded_bits = &self.decoded_bits;

        let mut state = 0;
        self.out_bytes[0..self.frame_param.psdu_size() + 1].fill(0);

        // First 7 bits initialize the scrambler state
        for i in 0..7 {
            if decoded_bits[i] > 0 {
                state |= 1 << (6 - i);
            }
        }

        // Bit 7 is the reserved SERVICE bit
        // Data starts at bit 8
        let mut feedback;
        let mut bit;

        for i in 7..self.frame_param.psdu_size() * 8 + 8 {
            feedback = u8::from((state & 64) > 0) ^ u8::from((state & 8) > 0);
            bit = feedback ^ (decoded_bits[i] & 1);
            // out_bytes[0] contains the SERVICE byte (bit 7 = reserved)
            // out_bytes[1..] contains the PSDU
            self.out_bytes[i / 8] |= bit << (i % 8);
            state = ((state << 1) & 0x7e) | feedback;
        }
    }

    /// Extract MPDUs from an A-MPDU.
    /// Works on descrambled out_bytes (not raw decoded_bits).
    fn extract_mpdus(&self) -> Vec<Vec<u8>> {
        let mut mpdus = Vec::new();
        let total_bytes = self.frame_param.psdu_size();
        // out_bytes[0] = SERVICE byte, out_bytes[1..] = PSDU data
        let data = &self.out_bytes[1..total_bytes + 1];

        let mut pos: usize = 0;
        while pos + 4 <= data.len() {
            let delim = &data[pos..pos + 4];

            // A-MPDU delimiter: 4 bytes
            // Byte layout (bit fields within the 32-bit delimiter):
            //   bit 0: EOF
            //   bit 1: reserved
            //   bits 2-15: MPDU length (14 bits): bits 2-3 high, bits 4-15 low
            //   bits 16-23: CRC-8 over bits 0-15
            //   bits 24-31: signature = 0x4E ('N')

            // Check signature
            if delim[3] != 0x4E {
                break;
            }

            // Extract bits 0..15 from the first two bytes for CRC-8 check
            let mut delim_bits_0_15 = [0u8; 16];
            for b in 0..16 {
                delim_bits_0_15[b] = (delim[b / 8] >> (b % 8)) & 1;
            }
            let crc_check = crc8(&delim_bits_0_15);
            if crc_check != delim[2] {
                break;
            }

            // MPDU length: bits 2-15 (14 bits)
            // bits 2-3 are the MSBs (×4096), bits 4-15 are the LSBs
            let length_hi = ((delim[0] >> 2) & 0x03) as usize;
            let length_lo = ((delim[0] >> 4) as usize) | ((delim[1] as usize) << 4);
            let mpdu_length = length_hi * 4096 + length_lo;

            // Extract MPDU bytes
            let mpdu_start = pos + 4;
            let mpdu_end = mpdu_start + mpdu_length;
            if mpdu_end > data.len() {
                break;
            }

            mpdus.push(data[mpdu_start..mpdu_end].to_vec());

            // A-MPDU subframes are padded to 4-byte boundaries
            let skip = 4 + ((mpdu_length + 3) / 4) * 4;
            pos += skip;
        }

        mpdus
    }
}

impl<I> Default for Decoder<I>
where
    I: CpuBufferReader<Item = u8>,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<I> Kernel for Decoder<I>
where
    I: CpuBufferReader<Item = u8>,
{
    async fn work(
        &mut self,
        io: &mut WorkIo,
        mio: &mut MessageOutputs,
        _b: &mut BlockMeta,
    ) -> Result<()> {
        let (mut input, in_tags) = self.input.slice_with_tags();

        if let Some((index, any)) = in_tags.iter().find_map(|x| match x {
            ItemTag {
                index,
                tag: Tag::NamedAny(n, any),
            } => {
                if n == "wifi_start" {
                    Some((index, any))
                } else {
                    None
                }
            }
            _ => None,
        }) {
            if *index == 0 {
                if !self.frame_complete {
                    warn!("decoder: previous frame not complete, canceling.");
                }
                let frame_param = any.downcast_ref::<FrameParam>().unwrap();
                if frame_param.n_symbols() <= MAX_SYM && frame_param.psdu_size() <= MAX_PSDU_SIZE {
                    self.frame_param = frame_param.clone();
                    self.copied = 0;
                    self.frame_complete = false;
                } else {
                    warn!("decoder: frame too large, dropping. ({:?})", frame_param);
                }
            } else {
                input = &input[0..*index];
            }
        }

        let max_i = input.len() / N_DATA_SC;
        let mut i = 0;

        while i < max_i {
            if self.copied < self.frame_param.n_symbols() {
                self.rx_symbols[(self.copied * N_DATA_SC)..((self.copied + 1) * N_DATA_SC)]
                    .copy_from_slice(&input[(i * N_DATA_SC)..((i + 1) * N_DATA_SC)]);
            }

            i += 1;
            self.copied += 1;

            if self.copied == self.frame_param.n_symbols() {
                self.frame_complete = true;

                self.decode();
                    if self.frame_param.aggregation {
                        // A-MPDU: extract individual MPDUs
                        let mpdus = self.extract_mpdus();
                        info!("decoder: A-MPDU extracted {} MPDUs from {:?} ({} syms, {} bytes)",
                            mpdus.len(), self.frame_param.mcs(), self.frame_param.n_symbols(), self.frame_param.psdu_size());
                        for mpdu in &mpdus {
                            // Output all MPDUs regardless of CRC
                            let blob = if mpdu.len() >= 4 {
                                let crc = crc32fast::hash(mpdu);
                                if crc == 558161692 {
                                    mpdu[..mpdu.len() - 4].to_vec()
                                } else {
                                    mpdu.clone()
                                }
                            } else {
                                mpdu.clone()
                            };
                            let mut rftap = vec![0; blob.len() + 12];
                            rftap[0..4].copy_from_slice("RFta".as_bytes());
                            rftap[4..6].copy_from_slice(&3u16.to_le_bytes());
                            rftap[6..8].copy_from_slice(&1u16.to_le_bytes());
                            rftap[8..12].copy_from_slice(&105u32.to_le_bytes());
                            rftap[12..].copy_from_slice(&blob);
                            mio.post("rx_frames", Pmt::Blob(blob)).await?;
                            mio.post("rftap", Pmt::Blob(rftap)).await?;
                        }
                    } else {
                        // Non-aggregated: SERVICE is 1 byte (out_bytes[0])
                        let psdu_end = self.frame_param.psdu_size() + 1;
                        // Try different padding lengths to find correct CRC boundary
                        let mut blob = None;
                        for padding in 0..4 {
                            let end = psdu_end - padding;
                            if end < 5 { break; }
                            let crc = crc32fast::hash(&self.out_bytes[1..end]);
                            if crc == 558161692 {
                                blob = Some(self.out_bytes[1..end - 4].to_vec());
                                break;
                            }
                        }
                        // If no valid CRC found, output full PSDU anyway
                        let blob = blob.unwrap_or_else(|| self.out_bytes[1..psdu_end].to_vec());
                        let mut rftap = vec![0; blob.len() + 12];
                        rftap[0..4].copy_from_slice("RFta".as_bytes());
                        rftap[4..6].copy_from_slice(&3u16.to_le_bytes());
                        rftap[6..8].copy_from_slice(&1u16.to_le_bytes());
                        rftap[8..12].copy_from_slice(&105u32.to_le_bytes());
                        rftap[12..].copy_from_slice(&blob);
                        mio.post("rx_frames", Pmt::Blob(blob)).await?;
                        mio.post("rftap", Pmt::Blob(rftap)).await?;
                    }

                i = max_i;
                break;
            }
        }

        self.input.consume(i * N_DATA_SC);
        if self.input.finished() && i == max_i {
            mio.post("rx_frames", Pmt::Finished).await?;
            io.finished = true;
        }

        Ok(())
    }
}
