//! Проверка HTTP-части распознавателя против локального сервера-заглушки (реальный API не нужен).

use rentflow::ocr::{ClaudeExtractor, Extractor};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Поднимает сервер, отвечающий один раз заданным статусом и телом. Возвращает базовый URL и принятый запрос.
async fn stub(
    status: &'static str,
    body: &'static str,
) -> (String, tokio::task::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let handle = tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            let n = sock.read(&mut chunk).await.unwrap();
            buf.extend_from_slice(&chunk[..n]);
            let text = String::from_utf8_lossy(&buf).to_string();
            if let Some(head_end) = text.find("\r\n\r\n") {
                let len = text[..head_end]
                    .lines()
                    .find_map(|l| {
                        l.to_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                if buf.len() >= head_end + 4 + len {
                    break;
                }
            }
            if n == 0 {
                break;
            }
        }
        let resp = format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        sock.write_all(resp.as_bytes()).await.unwrap();
        String::from_utf8_lossy(&buf).to_string()
    });
    (url, handle)
}

fn extractor(url: &str) -> ClaudeExtractor {
    ClaudeExtractor::new("test-key".into(), "claude-opus-5-5".into()).with_base_url(url)
}

#[tokio::test]
async fn success_returns_candidates_and_sends_key_and_version() {
    let (url, server) = stub(
        "200 OK",
        r#"{"stop_reason":"end_turn","content":[{"type":"thinking","thinking":""},{"type":"text","text":"{\"readings\":[{\"register\":\"t2\",\"value\":\"1179\",\"confidence\":\"high\"}]}"}]}"#,
    )
    .await;
    let c = extractor(&url).extract(&[1, 2, 3]).await.unwrap();
    assert_eq!(c.len(), 1);
    assert_eq!(c[0].register.as_deref(), Some("t2"));
    let request = server.await.unwrap().to_lowercase();
    assert!(request.contains("x-api-key: test-key"), "{request}");
    assert!(
        request.contains("anthropic-version: 2023-06-01"),
        "{request}"
    );
    assert!(request.contains("post /v1/messages"), "{request}");
}

#[tokio::test]
async fn api_error_becomes_a_manual_fallback_error() {
    let (url, _s) = stub(
        "529 Overloaded",
        r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
    )
    .await;
    let err = extractor(&url).extract(&[1]).await.unwrap_err();
    assert!(err.contains("529") && err.contains("Overloaded"), "{err}");
}

#[tokio::test]
async fn refusal_is_an_error_not_a_reading() {
    let (url, _s) = stub("200 OK", r#"{"stop_reason":"refusal","content":[]}"#).await;
    let err = extractor(&url).extract(&[1]).await.unwrap_err();
    assert!(err.contains("отказалась"), "{err}");
}

#[tokio::test]
async fn unparseable_answer_is_an_error() {
    let (url, _s) = stub(
        "200 OK",
        r#"{"stop_reason":"end_turn","content":[{"type":"text","text":"Это показание 761"}]}"#,
    )
    .await;
    assert!(extractor(&url).extract(&[1]).await.is_err());
}

#[tokio::test]
async fn unreachable_server_is_an_error() {
    let err = extractor("http://127.0.0.1:1")
        .extract(&[1])
        .await
        .unwrap_err();
    assert!(err.contains("сеть"), "{err}");
}
