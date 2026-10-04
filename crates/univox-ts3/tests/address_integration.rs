//! Address resolution integration: TSDNS query against a mock server.

use std::time::Duration;
use univox_ts3::address::{tsdns_lookup, parse, DEFAULT_PORT};

/// A minimal TSDNS answering "<port>" for any query.
async fn spawn_mock_tsdns(port_answer: &str) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let answer = format!("{port_answer}\n");
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { break };
            let answer = answer.clone();
            tokio::spawn(async move {
                use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
                let mut rd = tokio::io::BufReader::new(&mut sock);
                let mut line = String::new();
                if rd.read_line(&mut line).await.is_ok() {
                    let _ = sock.write_all(answer.as_bytes()).await;
                    let _ = sock.flush().await;
                }
            });
        }
    });
    port
}

#[tokio::test(flavor = "multi_thread")]
async fn tsdns_answers_with_port() {
    let tsdns_port = spawn_mock_tsdns("9989").await;
    let port = tsdns_lookup("127.0.0.1", tsdns_port, "/lobby")
        .await
        .expect("tsdns lookup")
        .expect("should answer a port");
    assert_eq!(port, 9989);
}

#[tokio::test(flavor = "multi_thread")]
async fn tsdns_zero_means_default() {
    let tsdns_port = spawn_mock_tsdns("0").await;
    let port = tsdns_lookup("127.0.0.1", tsdns_port, "").await.expect("tsdns");
    assert_eq!(port, None, "answer 0 means 'no special port'");
}

#[tokio::test(flavor = "multi_thread")]
async fn tsdns_unreachable_yields_error() {
    // Nothing listens on that port.
    let r = tsdns_lookup("127.0.0.1", 1, "").await;
    assert!(
        r.is_err() || matches!(r, Ok(None)),
        "unexpected: {r:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn parse_defaults_preserved() {
    let a = parse("voice.example.com").unwrap();
    assert!(!a.port_explicit);
    assert_eq!(a.port, DEFAULT_PORT);
    let a = parse("voice.example.com:2000").unwrap();
    assert!(a.port_explicit);
    assert_eq!(a.port, 2000);
}

#[tokio::test(flavor = "multi_thread")]
async fn resolve_port_falls_back_to_default() {
    // No SRV (stub) and no TSDNS on the default test port here: exercise
    // the fallback with a guaranteed-free tsdns port.
    let free = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let port = univox_ts3::address::resolve_port("127.0.0.1", "", free)
        .await
        .expect("resolve");
    assert_eq!(port, DEFAULT_PORT);
}

#[tokio::test(flavor = "multi_thread")]
async fn resolve_port_uses_tsdns() {
    let tsdns_port = spawn_mock_tsdns("9999").await;
    let port = univox_ts3::address::resolve_port("127.0.0.1", "", tsdns_port)
        .await
        .expect("resolve");
    assert_eq!(port, 9999);
}

#[tokio::test(flavor = "multi_thread")]
async fn parse_is_deterministic() {
    let a = parse("ts3server://h?port=5&port=6").unwrap();
    assert_eq!(a.port, 6, "last wins");
    let _ = Duration::from_secs(0); // keep the import if trimmed
}
