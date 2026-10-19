//! Reading a server-sent event stream: bytes in, one frame per event out.

/// One event of a stream: its `id` line, when it had one, and its `data` lines joined by `\n`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Frame {
    pub id: Option<String>,
    pub data: String,
}

/// Splits a stream into frames as its bytes arrive, in pieces of any size. Comment lines (`:`)
/// and fields other than `id` and `data` are skipped; a blank line ends a frame, and a frame with
/// no data is dropped, as a browser's `EventSource` drops it.
#[derive(Debug, Default)]
pub(crate) struct FrameReader {
    pending: Vec<u8>,
    id: Option<String>,
    data: Vec<String>,
}

impl FrameReader {
    /// Takes the next piece of the stream and answers the frames it completed.
    pub(crate) fn push(&mut self, bytes: &[u8]) -> Vec<Frame> {
        self.pending.extend_from_slice(bytes);
        let mut frames = Vec::new();
        while let Some(end) = self.pending.iter().position(|byte| *byte == b'\n') {
            let mut line: Vec<u8> = self.pending.drain(..=end).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            let line = String::from_utf8_lossy(&line).into_owned();
            if let Some(frame) = self.line(&line) {
                frames.push(frame);
            }
        }
        frames
    }

    fn line(&mut self, line: &str) -> Option<Frame> {
        if line.is_empty() {
            let id = self.id.take();
            if self.data.is_empty() {
                return None;
            }
            let data = std::mem::take(&mut self.data).join("\n");
            return Some(Frame { id, data });
        }
        if line.starts_with(':') {
            return None;
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "id" => self.id = Some(value.to_owned()),
            "data" => self.data.push(value.to_owned()),
            _ => {}
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_are_cut_at_blank_lines_whatever_the_pieces() {
        let stream =
            b": keep-alive\n\nid: 7\ndata: {\"a\":1}\n\nid: 8\r\ndata: x\r\ndata: y\r\n\r\n";
        let whole = FrameReader::default().push(stream);
        let mut pieces = FrameReader::default();
        let byte_by_byte: Vec<Frame> = stream
            .iter()
            .flat_map(|byte| pieces.push(std::slice::from_ref(byte)))
            .collect();
        let expected = vec![
            Frame {
                id: Some("7".to_owned()),
                data: "{\"a\":1}".to_owned(),
            },
            Frame {
                id: Some("8".to_owned()),
                data: "x\ny".to_owned(),
            },
        ];
        assert_eq!(whole, expected);
        assert_eq!(byte_by_byte, expected);
    }

    #[test]
    fn a_frame_without_data_and_unknown_fields_are_skipped() {
        let frames = FrameReader::default().push(b"id: 3\n\nevent: x\nretry: 10\ndata: d\n\n");
        assert_eq!(
            frames,
            vec![Frame {
                id: None,
                data: "d".to_owned()
            }]
        );
    }
}
