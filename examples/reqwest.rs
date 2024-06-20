use futures_util::stream::TryStreamExt;
use nd_tokio_sse_codec::SseCodec;
use tokio_stream::StreamExt;
use tokio_util::codec::FramedRead;
use tokio_util::io::StreamReader;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let client = reqwest::Client::new();

    let stream = client
        .get("https://sse.dev/test")
        .send()
        .await
        .expect("failed to send request")
        .error_for_status()
        .expect("invalid http status")
        .bytes_stream()
        .map_err(std::io::Error::other);
    let stream_reader = StreamReader::new(stream);
    let codec = SseCodec::new();
    let mut reader = FramedRead::new(stream_reader, codec);

    // This will go on forever, printing an event every 2 seconds...
    while let Some(event) = reader.next().await {
        let event = event.expect("invalid event");

        println!("message: {}", event.data.expect("event had no message"));
    }
}
