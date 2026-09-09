use futures::StreamExt;
use serde_json::{json, Value};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use vybrid::client::groq::{GroqClient, RequestTuning};
use vybrid::conversation::Conversation;

/// A single local HTTP exchange: no API keys, external server, or paid requests.
async fn mock_response(
    body: String,
    content_type: &str,
    split: Option<usize>,
) -> (GroqClient, tokio::task::JoinHandle<Value>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let headers = format!("HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
    let server = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(5), async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buf = [0; 4096];
            let payload = loop {
                let count = socket.read(&mut buf).await.unwrap();
                assert!(
                    count > 0,
                    "client disconnected before completing the request"
                );
                request.extend_from_slice(&buf[..count]);
                if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..end]);
                    let length: usize = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse().unwrap())
                        })
                        .unwrap();
                    if request.len() >= end + 4 + length {
                        break serde_json::from_slice(&request[end + 4..end + 4 + length]).unwrap();
                    }
                }
            };
            socket.write_all(headers.as_bytes()).await.unwrap();
            let split = split.unwrap_or(body.len());
            socket.write_all(&body.as_bytes()[..split]).await.unwrap();
            socket.flush().await.unwrap();
            if split < body.len() {
                tokio::time::sleep(Duration::from_millis(25)).await;
                socket.write_all(&body.as_bytes()[split..]).await.unwrap();
            }
            payload
        })
        .await
        .expect("local provider exchange timed out")
    });
    let client = GroqClient::new(
        "local-test".into(),
        format!("http://{address}/v1"),
        "local-test".into(),
        RequestTuning::default(),
    );
    (client, server)
}

#[tokio::test]
async fn streamed_tool_arguments_preserve_fragmented_unicode() {
    let arguments = json!({"old_string": "café 🦀"}).to_string();
    let chunk = json!({"choices": [{"index": 0, "delta": {"tool_calls": [{
        "index": 0, "id": "call_1", "type": "function", "function": {"name": "edit_file", "arguments": arguments}
    }]}, "finish_reason": "tool_calls"}]});
    let body = format!("data: {chunk}\r\n\r\ndata: [DONE]\r\n\r\n");
    let split = body.find('é').unwrap() + 1;
    let (client, server) = mock_response(body, "text/event-stream", Some(split)).await;
    let conversation = Conversation::new("system");
    let mut stream = client
        .chat_stream(&conversation.messages, None)
        .await
        .unwrap();
    let mut received = String::new();
    while let Some(chunk) = stream.next().await {
        for choice in chunk.unwrap().choices {
            for call in choice.delta.tool_calls.unwrap_or_default() {
                if let Some(function) = call.function {
                    received.push_str(&function.arguments.unwrap_or_default());
                }
            }
        }
    }
    assert_eq!(received, arguments);
    server.await.unwrap();
}

#[tokio::test]
async fn stream_eof_remains_an_error_after_a_finish_reason() {
    let body = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"},\"finish_reason\":\"stop\"}]}\n\n";
    let (client, server) = mock_response(body.into(), "text/event-stream", None).await;
    let conversation = Conversation::new("system");
    let mut stream = client
        .chat_stream(&conversation.messages, None)
        .await
        .unwrap();
    assert!(stream.next().await.unwrap().is_ok());
    let error = stream.next().await.unwrap().unwrap_err();
    assert!(error.to_string().contains("[DONE]"));
    server.await.unwrap();
}

#[tokio::test]
async fn summary_requests_reject_incomplete_finish_reasons() {
    for finish in [json!("length"), Value::Null, json!("stop")] {
        let body = json!({"choices": [{"message": {"role": "assistant", "content": "Summary"}, "finish_reason": finish}]}).to_string();
        let (client, server) = mock_response(body, "application/json", None).await;
        let conversation = Conversation::new("system");
        let result = client
            .with_completion_limit(2048)
            .chat(&conversation.messages, None)
            .await;
        assert_eq!(result.is_ok(), finish == "stop");
        assert_eq!(server.await.unwrap()["max_tokens"], 2048);
    }
}
