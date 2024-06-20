use tokio_util::bytes::Buf;
use tokio_util::bytes::BytesMut;
use tokio_util::codec::Decoder;

/// An sse codec error
#[derive(Debug)]
pub enum SseCodecError {
    /// A line was not valid utf8.
    InvalidUtf8(std::str::Utf8Error),

    /// An IO error occurred.
    Io(std::io::Error),
}

impl std::fmt::Display for SseCodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::InvalidUtf8(_) => write!(f, "a line was not valid utf8"),
            Self::Io(_) => write!(f, "an I/O error occured"),
        }
    }
}

impl std::error::Error for SseCodecError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidUtf8(error) => Some(error),
            Self::Io(error) => Some(error),
        }
    }
}

impl From<std::io::Error> for SseCodecError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

/// An sse event
#[derive(Debug, PartialEq)]
pub struct SseEvent {
    /// The event field
    pub event: Option<String>,

    /// The data field
    pub data: Option<String>,

    /// The id field
    pub id: Option<String>,

    /// The retry field
    pub retry: Option<u64>,
}

/// An sse codec
#[derive(Debug)]
pub struct SseCodec {
    // Check if the last newline was a \r.
    last_newline_cr: bool,

    /// The event field
    event: Option<String>,

    /// The data field
    data: Option<String>,

    /// The id field
    id: Option<String>,

    /// The retry field
    retry: Option<u64>,
}

impl SseCodec {
    /// Make a new SSE Event decoder.
    pub fn new() -> Self {
        Self {
            last_newline_cr: false,
            event: None,
            data: None,
            id: None,
            retry: None,
        }
    }
}

impl Decoder for SseCodec {
    type Item = SseEvent;
    type Error = SseCodecError;

    fn decode(&mut self, bytes: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        loop {
            // We need at least 1 byte to work with.
            if bytes.is_empty() {
                return Ok(None);
            }

            // Need to handle: \n, \r\n, \r
            // If the last newline was \r, trim the \n if one occurs.
            if self.last_newline_cr && bytes[0] == b'\n' {
                bytes.advance(1);
                self.last_newline_cr = false;
            }

            let newline_index = match bytes.iter().position(|b| *b == b'\r' || *b == b'\n') {
                Some(newline_index) => {
                    // To handle a multi-byte newline,
                    // we need to discard the next byte if the current newline is a \r and the next byte is a \n.
                    // However, doing that here will lead to issues if a \r newline is the last newline in a stream.
                    // Instead, set a flag and skip the extra \n then if needed.
                    if bytes[newline_index] == b'\r' {
                        self.last_newline_cr = true;
                    }

                    newline_index
                }
                None => {
                    return Ok(None);
                }
            };

            let line =
                std::str::from_utf8(&bytes[..newline_index]).map_err(SseCodecError::InvalidUtf8)?;
            let advance = line.len() + 1;

            if line.is_empty() {
                bytes.advance(advance);

                if let Some(data) = self.data.as_mut() {
                    // Trim trailing \n, per-spec.
                    if data.ends_with('\n') {
                        data.pop();
                    }
                }

                return Ok(Some(SseEvent {
                    event: self.event.take(),
                    data: self.data.take(),
                    id: self.id.take(),
                    retry: self.retry.take(),
                }));
            }

            let colon_index = line.bytes().position(|b| b == b':');

            let (field, value) = match colon_index {
                Some(0) => {
                    // TODO: Consider letting user know about comments
                    bytes.advance(advance);
                    continue;
                }
                Some(index) => {
                    let (field, mut value) = line.split_at(index);
                    // Trim the :
                    value = &value[1..];

                    // If it has a starting space, trim that.
                    if value.as_bytes().first() == Some(&b' ') {
                        value = &value[1..];
                    }

                    (field, value)
                }
                None => (line, ""),
            };

            match field {
                "event" => {
                    // Overwrite old buffer, per spec.
                    self.event = Some(value.into());
                }
                "data" => {
                    // Append to data buffer and append \n, per spec.
                    let data = self.data.get_or_insert_with(String::new);
                    data.push_str(value);
                    data.push('\n');
                }
                "id" => {
                    // Ignore if id has interior NULs, per spec.
                    if !value.contains('\0') {
                        self.id = Some(value.into());
                    }
                }
                "retry" => {
                    // Ignore if not all ascii digits, per spec.
                    // Also, attempt to parse into usable integer format,
                    // which is implementation-defined by the spec,
                    // as long as it can hold a few seconds in milliseconds.
                    if let Ok(value) = value.parse() {
                        self.retry = Some(value);
                    }
                }
                _ => {
                    // Ignore other fields.
                }
            }

            bytes.advance(advance);
        }
    }

    fn decode_eof(&mut self, buf: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        match self.decode(buf)? {
            Some(frame) => Ok(Some(frame)),
            None => {
                // Decode will only return None if it is passed an empty buffer or not have a trailing newline.
                // Per-spec, buffered event parts should be discarded if the stream is terminated without a trailing newline.
                Ok(None)
            }
        }
    }
}

impl Default for SseCodec {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use tokio_stream::StreamExt;
    use tokio_util::codec::FramedRead;

    #[tokio::test]
    async fn corpus() {
        let mut dir_iter = tokio::fs::read_dir("corpus")
            .await
            .expect("failed to iter dir");

        while let Some(entry) = dir_iter
            .next_entry()
            .await
            .expect("failed to read next entry")
        {
            let test_data = tokio::fs::read_to_string(entry.path())
                .await
                .expect("failed to read test data");
            let mut reader = FramedRead::new(test_data.as_bytes(), SseCodec::new());

            while let Some(event) = reader.next().await {
                let event = event.expect("failed to parse");
                dbg!(event);
            }
        }
    }

    #[tokio::test]
    async fn field_colon_space() {
        let test_data = "data:test\n\ndata: test\n\n";
        let mut reader = FramedRead::new(test_data.as_bytes(), SseCodec::new());
        let event_1 = reader
            .next()
            .await
            .expect("missing event 1")
            .expect("failed to parse");
        let expected_event = SseEvent {
            event: None,
            data: Some("test".into()),
            id: None,
            retry: None,
        };
        assert!(event_1 == expected_event);

        let event_2 = reader
            .next()
            .await
            .expect("missing event 2")
            .expect("failed to parse");
        assert!(event_2 == expected_event);

        let no_event_3 = reader.next().await.is_none();
        assert!(no_event_3);
    }

    #[tokio::test]
    async fn field_colon_space_r() {
        let test_data = "data:test\r\rdata: test\r\r";
        let mut reader = FramedRead::new(test_data.as_bytes(), SseCodec::new());
        let event_1 = reader
            .next()
            .await
            .expect("missing event 1")
            .expect("failed to parse");
        let expected_event = SseEvent {
            event: None,
            data: Some("test".into()),
            id: None,
            retry: None,
        };
        assert!(event_1 == expected_event);

        let event_2 = reader
            .next()
            .await
            .expect("missing event 2")
            .expect("failed to parse");
        assert!(event_2 == expected_event);

        let no_event_3 = reader.next().await.is_none();
        assert!(no_event_3);
    }

    #[tokio::test]
    async fn field_colon_space_rn() {
        let test_data = "data:test\r\n\r\ndata: test\r\n\r\n";
        let mut reader = FramedRead::new(test_data.as_bytes(), SseCodec::new());
        let event_1 = reader
            .next()
            .await
            .expect("missing event 1")
            .expect("failed to parse");
        let expected_event = SseEvent {
            event: None,
            data: Some("test".into()),
            id: None,
            retry: None,
        };
        assert!(event_1 == expected_event);

        let event_2 = reader
            .next()
            .await
            .expect("missing event 2")
            .expect("failed to parse");
        assert!(event_2 == expected_event);

        let no_event_3 = reader.next().await.is_none();
        assert!(no_event_3);
    }

    #[tokio::test]
    async fn trailing_nl() {
        let test_data = "data\n\ndata\ndata\n\ndata:";
        let mut reader = FramedRead::new(test_data.as_bytes(), SseCodec::new());
        let event_1 = reader
            .next()
            .await
            .expect("missing event 1")
            .expect("failed to parse");
        let expected_event_1 = SseEvent {
            event: None,
            data: Some("".into()),
            id: None,
            retry: None,
        };
        assert!(event_1 == expected_event_1);

        let event_2 = reader
            .next()
            .await
            .expect("missing event 2")
            .expect("failed to parse");
        let expected_event_2 = SseEvent {
            event: None,
            data: Some("\n".into()),
            id: None,
            retry: None,
        };
        assert!(event_2 == expected_event_2);

        let no_event_3 = reader.next().await.is_none();
        assert!(no_event_3);
    }
}
