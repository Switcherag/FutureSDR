//! Append `Pmt::Blob` messages to a file, byte-for-byte. Used as the tail of
//! a network-extractor chain to dump per-frame ASCII records (the extractor
//! already terminates each record with `\n`).

use futuresdr::prelude::*;
use std::fs::{File, OpenOptions};
use std::io::Write;

#[derive(Block)]
#[message_inputs(r#in)]
pub struct MessageFileSink {
    path: String,
    file: Option<File>,
}

impl MessageFileSink {
    pub fn new(path: String) -> Self {
        Self { path, file: None }
    }

    async fn r#in(
        &mut self,
        io: &mut WorkIo,
        _mio: &mut MessageOutputs,
        _meta: &mut BlockMeta,
        p: Pmt,
    ) -> Result<Pmt> {
        match p {
            Pmt::Blob(b) => {
                if let Some(f) = self.file.as_mut() {
                    let _ = f.write_all(&b);
                    let _ = f.flush();
                }
            }
            Pmt::String(s) => {
                if let Some(f) = self.file.as_mut() {
                    let _ = f.write_all(s.as_bytes());
                    let _ = f.flush();
                }
            }
            Pmt::Finished => {
                io.finished = true;
            }
            _ => {}
        }
        Ok(Pmt::Ok)
    }
}

impl Kernel for MessageFileSink {
    async fn init(&mut self, _mio: &mut MessageOutputs, _b: &mut BlockMeta) -> Result<()> {
        let f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        self.file = Some(f);
        Ok(())
    }
}

plugin_api::export_plugin! {
    name: "MessageFileSink",
    description: "Append Pmt::Blob/String messages to a file (line-oriented).",
    config: String,
    create: |cfg, _id| {
        MessageFileSink::new(cfg)
    }
}
