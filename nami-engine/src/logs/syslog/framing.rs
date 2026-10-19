//! Syslog over TCP framing (RFC 6587), detected per frame: octet counting
//! (`<length> <message>`, the frame starts with a digit) or non-transparent
//! framing (the message ends at LF, optional CR before it; it starts with
//! `<`). Separators between frames (LF, CR, spaces, NUL) are skipped. A
//! frame may be at most [`MAX_FRAME_BYTES`]; framing errors are fatal for
//! the connection, because a stream cannot be resynchronised after them.

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader};

pub const MAX_FRAME_BYTES: usize = 64 * 1024;
/// "65536" is the longest valid octet count.
const MAX_LENGTH_DIGITS: usize = 5;

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("frame longer than 64 KiB")]
    TooLarge,
    #[error("invalid octet count")]
    InvalidLength,
    #[error("connection closed in the middle of a frame")]
    Truncated,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub struct FrameReader<R> {
    reader: BufReader<R>,
}

impl<R: AsyncRead + Unpin> FrameReader<R> {
    pub fn new(inner: R) -> Self {
        FrameReader {
            reader: BufReader::new(inner),
        }
    }

    /// The next frame without its framing; `None` once the stream ends
    /// between frames.
    pub async fn next_frame(&mut self) -> Result<Option<Vec<u8>>, FrameError> {
        let first = loop {
            let buf = self.reader.fill_buf().await?;
            if buf.is_empty() {
                return Ok(None);
            }
            let separators = buf
                .iter()
                .take_while(|b| matches!(b, b'\n' | b'\r' | b' ' | b'\t' | 0))
                .count();
            let first = buf.get(separators).copied();
            self.reader.consume(separators);
            if let Some(first) = first {
                break first;
            }
        };
        if first.is_ascii_digit() {
            self.octet_counted().await.map(Some)
        } else {
            self.newline_delimited().await.map(Some)
        }
    }

    async fn read_byte(&mut self) -> Result<Option<u8>, FrameError> {
        let buf = self.reader.fill_buf().await?;
        let Some(&byte) = buf.first() else {
            return Ok(None);
        };
        self.reader.consume(1);
        Ok(Some(byte))
    }

    async fn octet_counted(&mut self) -> Result<Vec<u8>, FrameError> {
        let mut len = 0usize;
        let mut digits = 0;
        loop {
            match self.read_byte().await?.ok_or(FrameError::Truncated)? {
                digit @ b'0'..=b'9' if digits < MAX_LENGTH_DIGITS => {
                    len = len * 10 + usize::from(digit - b'0');
                    digits += 1;
                }
                b' ' if digits > 0 => break,
                _ => return Err(FrameError::InvalidLength),
            }
        }
        if len == 0 {
            return Err(FrameError::InvalidLength);
        }
        if len > MAX_FRAME_BYTES {
            return Err(FrameError::TooLarge);
        }
        let mut frame = vec![0u8; len];
        self.reader
            .read_exact(&mut frame)
            .await
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::UnexpectedEof => FrameError::Truncated,
                _ => FrameError::Io(e),
            })?;
        Ok(frame)
    }

    async fn newline_delimited(&mut self) -> Result<Vec<u8>, FrameError> {
        let mut frame = Vec::new();
        (&mut self.reader)
            .take(MAX_FRAME_BYTES as u64 + 1)
            .read_until(b'\n', &mut frame)
            .await?;
        if frame.last() == Some(&b'\n') {
            frame.pop();
            if frame.last() == Some(&b'\r') {
                frame.pop();
            }
            return Ok(frame);
        }
        if frame.len() > MAX_FRAME_BYTES {
            return Err(FrameError::TooLarge);
        }
        // The stream ended without a final LF: that is the last frame.
        Ok(frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn frames(input: &[u8]) -> Result<Vec<Vec<u8>>, FrameError> {
        let mut reader = FrameReader::new(input);
        let mut frames = Vec::new();
        while let Some(frame) = reader.next_frame().await? {
            frames.push(frame);
        }
        Ok(frames)
    }

    #[tokio::test]
    async fn newline_and_octet_counted_frames_can_be_mixed() {
        let input = b"<1>1 a\r\n\n<2>1 b\n6 <3>1 c10 <4>1 d e f\n \x00<5>1 last";
        let got = frames(input).await.unwrap();
        let expected: Vec<&[u8]> =
            vec![b"<1>1 a", b"<2>1 b", b"<3>1 c", b"<4>1 d e f", b"<5>1 last"];
        assert_eq!(got, expected);
    }

    #[tokio::test]
    async fn a_64_kib_frame_fits_in_both_framings() {
        let body = vec![b'x'; MAX_FRAME_BYTES];
        let mut newline = body.clone();
        newline.push(b'\n');
        assert_eq!(frames(&newline).await.unwrap(), vec![body.clone()]);
        let mut counted = format!("{MAX_FRAME_BYTES} ").into_bytes();
        counted.extend(&body);
        assert_eq!(frames(&counted).await.unwrap(), vec![body]);
    }

    #[tokio::test]
    async fn framing_errors_are_reported() {
        let too_long_line = vec![b'<'; MAX_FRAME_BYTES + 1];
        let cases: Vec<(Vec<u8>, &str)> = vec![
            (b"123456 x".to_vec(), "InvalidLength"),
            (b"99999999999999999999 x".to_vec(), "InvalidLength"),
            (b"65537 x".to_vec(), "TooLarge"),
            (b"0 x".to_vec(), "InvalidLength"),
            (b"12x".to_vec(), "InvalidLength"),
            (b"12".to_vec(), "Truncated"),
            (b"10 <1>1 a".to_vec(), "Truncated"),
            (too_long_line, "TooLarge"),
        ];
        for (input, expected) in cases {
            let error = frames(&input).await.unwrap_err();
            assert!(
                format!("{error:?}").starts_with(expected),
                "{:?}: {error:?}",
                String::from_utf8_lossy(&input[..input.len().min(30)])
            );
        }
    }

    #[tokio::test]
    async fn an_empty_stream_has_no_frames() {
        assert!(frames(b"").await.unwrap().is_empty());
        assert!(frames(b"\n\r\n  \x00").await.unwrap().is_empty());
    }
}
