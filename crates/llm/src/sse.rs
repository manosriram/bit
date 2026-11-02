/// Splits `data:` payloads out of an SSE byte stream.
#[derive(Default)]
pub struct Sse {
    buf: Vec<u8>,
}

impl Sse {
    /// Bytes are buffered until a newline, so a UTF-8 character split across chunks
    /// reaches `f` whole.
    pub fn feed<E>(
        &mut self,
        chunk: &[u8],
        mut f: impl FnMut(&[u8]) -> Result<(), E>,
    ) -> Result<(), E> {
        self.buf.extend_from_slice(chunk);
        let mut start = 0;
        while let Some(n) = self.buf[start..].iter().position(|&b| b == b'\n') {
            let line = &self.buf[start..start + n];
            start += n + 1;
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            if let Some(data) = line.strip_prefix(b"data:") {
                f(data.trim_ascii_start())?;
            }
        }
        self.buf.drain(..start);
        Ok(())
    }
}
