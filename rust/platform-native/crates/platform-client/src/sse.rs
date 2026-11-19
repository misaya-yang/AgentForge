//! Incremental byte-oriented SSE framing. UTF-8 is decoded only after complete lines.
use crate::Error;
#[derive(Default)]
pub struct Decoder {
    pending: Vec<u8>,
    data: Vec<String>,
    frame_bytes: usize,
}
impl Decoder {
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<String>, Error> {
        if self.pending.len().saturating_add(bytes.len()) > 1_048_576 {
            return Err(Error::Response);
        }
        self.pending.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some(pos) = self.pending.iter().position(|b| *b == b'\n') {
            let line: Vec<_> = self.pending.drain(..=pos).collect();
            let line = std::str::from_utf8(&line[..line.len() - 1])
                .map_err(|_| Error::Response)?
                .trim_end_matches('\r');
            self.line(line, &mut out)?;
        }
        Ok(out)
    }
    fn line(&mut self, line: &str, out: &mut Vec<String>) -> Result<(), Error> {
        self.frame_bytes = self.frame_bytes.saturating_add(line.len());
        if self.frame_bytes > 1_048_576 {
            return Err(Error::Response);
        }
        if line.is_empty() {
            if !self.data.is_empty() {
                out.push(self.data.join("\n"));
                self.data.clear();
            }
            self.frame_bytes = 0;
        } else if let Some(data) = line.strip_prefix("data:") {
            self.data
                .push(data.strip_prefix(' ').unwrap_or(data).into());
        } else if line == "data" {
            self.data.push(String::new());
        }
        Ok(())
    }
    pub fn finish(mut self) -> Result<Vec<String>, Error> {
        let mut out = Vec::new();
        let bytes = std::mem::take(&mut self.pending);
        if !bytes.is_empty() {
            self.line(
                std::str::from_utf8(&bytes)
                    .map_err(|_| Error::Response)?
                    .trim_end_matches('\r'),
                &mut out,
            )?;
        }
        self.line("", &mut out)?;
        Ok(out)
    }
}
