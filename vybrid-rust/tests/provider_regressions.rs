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

#[tokio::test]
async fn incomplete_compaction_archives_exact_history_and_keeps_the_recent_tool_group() {
    use vybrid::client::groq::{FunctionCall, Message, ToolCall};
    use vybrid::compaction::{Compaction, Compactor};

    let mut c = Conversation::new("system");
    c.add_task_message("Keep diagnostics in Spanish.");
    for i in 0..12 {
        c.add_user_message(&format!("decision {i} {}", "ñ".repeat(160)));
    }
    let end = c.messages.len();
    c.add_assistant_message(Message {
        role: "assistant".into(),
        content: None,
        tool_calls: Some(vec![ToolCall {
            id: "call-1".into(),
            call_type: "function".into(),
            function: FunctionCall {
                name: "read_file".into(),
                arguments: "{}".into(),
            },
        }]),
        tool_call_id: None,
    });
    c.add_tool_result("call-1", "resultado");
    for i in 0..7 {
        c.add_user_message(&format!("recent {i}"));
    }
    let original = serde_json::to_value(&c.messages[1..end]).unwrap();
    let tail = serde_json::to_value(&c.messages[end..]).unwrap();
    let before = c.estimate_active_tokens();
    let dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
    let mut compactor = Compactor::new(dir.clone());
    let body = json!({"choices":[{"message":{"role":"assistant","content":"partial answer"},"finish_reason":"length"}]}).to_string();
    let (client, exchange) = mock_response(body, "application/json", None).await;
    let Compaction::Archived(path) = compactor
        .compact(Some(&client), &mut c, "focus")
        .await
        .unwrap()
    else {
        panic!("expected local compaction");
    };
    assert!(c.estimate_active_tokens() < before);
    let request = exchange.await.unwrap();
    assert!(request["max_tokens"].as_u64().unwrap() < 2048);
    assert!(c.messages[1]
        .content
        .as_ref()
        .unwrap()
        .contains(path.to_str().unwrap()));
    assert!(c
        .messages
        .iter()
        .any(|m| m.content.as_deref() == Some("Keep diagnostics in Spanish.")));
    let archived: Vec<Value> = std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(json!(archived), original);
    assert_eq!(
        serde_json::to_value(&c.messages[c.messages.len() - 9..]).unwrap(),
        tail
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn oversized_old_message_survives_archive_failure_and_can_be_compacted_offline() {
    use vybrid::compaction::{Compaction, Compactor};
    let dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
    std::fs::write(&dir, "blocks creation of the archive directory").unwrap();
    let mut compactor = Compactor::new(dir.clone());
    let mut c = Conversation::new("system");
    let large = "🦀".repeat(25_000);
    c.add_user_message(&large);
    for i in 0..8 {
        c.add_user_message(&format!("recent {i}"));
    }
    assert!(c.compactable_transcript(8).is_none());
    let before = serde_json::to_value(&c.messages).unwrap();
    assert!(compactor.compact(None, &mut c, "").await.is_err());
    assert_eq!(serde_json::to_value(&c.messages).unwrap(), before);
    std::fs::remove_file(&dir).unwrap();

    let Compaction::Archived(path) = compactor.compact(None, &mut c, "").await.unwrap() else {
        panic!("expected an archive");
    };
    let stored: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(stored["content"], large);
    assert!(c.estimate_active_tokens() < 300);
    assert!(matches!(
        compactor.compact(None, &mut c, "").await.unwrap(),
        Compaction::Unchanged
    ));
    assert_eq!(
        std::fs::read_dir(dir.join("compactions")).unwrap().count(),
        1
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn completed_compaction_keeps_the_models_summary_without_creating_an_archive() {
    use vybrid::compaction::{Compaction, Compactor};
    let dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
    let mut compactor = Compactor::new(dir.clone());
    let mut c = Conversation::new("system");
    for i in 0..24 {
        c.add_user_message(&format!("round {i} {}", "x".repeat(480)));
    }
    let body = json!({"choices":[{"message":{"role":"assistant","content":"Keep parser.rs compatible."},"finish_reason":"stop"}]}).to_string();
    let (client, exchange) = mock_response(body, "application/json", None).await;
    let before = c.estimate_active_tokens();
    let result = compactor
        .compact(Some(&client), &mut c, "Remember parser.rs")
        .await
        .unwrap();
    assert!(matches!(result, Compaction::Summarized));
    assert!(c.estimate_active_tokens() < before);
    assert!(c.messages[1]
        .content
        .as_ref()
        .unwrap()
        .contains("Keep parser.rs compatible."));
    assert!(exchange.await.unwrap()["messages"][1]["content"]
        .as_str()
        .unwrap()
        .contains("Remember parser.rs"));
    assert!(!dir.exists());
}
