use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};

/// Strict JSONL framing as Pi's docs require: records end at LF only, an
/// optional CR before it is dropped, and U+2028/U+2029 inside strings are data.
pub struct JsonlReader<R> {
    inner: BufReader<R>,
}

impl<R: AsyncRead + Unpin> JsonlReader<R> {
    pub fn new(reader: R) -> Self {
        Self {
            inner: BufReader::new(reader),
        }
    }

    /// The next record without its line ending, or `None` at end of stream.
    /// A final record without a trailing LF is still returned.
    pub async fn next_record(&mut self) -> std::io::Result<Option<Vec<u8>>> {
        let mut buf = Vec::new();
        let n = self.inner.read_until(b'\n', &mut buf).await?;
        if n == 0 {
            return Ok(None);
        }
        if buf.last() == Some(&b'\n') {
            buf.pop();
            if buf.last() == Some(&b'\r') {
                buf.pop();
            }
        }
        Ok(Some(buf))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn records(input: &[u8]) -> Vec<Vec<u8>> {
        let mut r = JsonlReader::new(input);
        let mut out = Vec::new();
        while let Some(rec) = r.next_record().await.unwrap() {
            out.push(rec);
        }
        out
    }

    #[tokio::test]
    async fn splits_on_lf_only_and_strips_cr() {
        let input = "{\"a\":1}\r\n{\"b\":\"x\u{2028}y\u{2029}z\"}\n{\"c\":3}".as_bytes();
        let recs = records(input).await;
        assert_eq!(recs.len(), 3);
        assert_eq!(recs[0], br#"{"a":1}"#);
        let v: serde_json::Value = serde_json::from_slice(&recs[1]).unwrap();
        assert_eq!(v["b"], "x\u{2028}y\u{2029}z");
        assert_eq!(recs[2], br#"{"c":3}"#);
    }

    #[tokio::test]
    async fn empty_stream_has_no_records() {
        assert!(records(b"").await.is_empty());
    }
}
