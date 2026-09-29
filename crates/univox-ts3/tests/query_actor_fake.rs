//! Deterministic fake-server tests for the query actor.

use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

use univox_ts3::query::{QueryConnection, QueryOptions};
use univox_ts3_proto::Command;

async fn spawn_fake_server() -> (u16, tokio::sync::mpsc::Sender<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx_out, mut rx_out) = tokio::sync::mpsc::channel::<String>(16);
    let (tx_in, mut rx_in) = tokio::sync::mpsc::channel::<String>(16);
    tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        let (r, mut w) = sock.into_split();
        // Greeting
        w.write_all(b"TS3\nWelcome to the TeamSpeak 3 ServerQuery interface.\n").await.unwrap();
        let mut reader = BufReader::new(r);
        let mut line = String::new();
        loop {
            tokio::select! {
                n = reader.read_line(&mut line) => {
                    if n.unwrap_or(0) == 0 { break; }
                    let cmd = line.trim().to_string();
                    line.clear();
                    let _ = tx_out.send(cmd.clone()).await;
                    if let Some(push) = cmd.strip_prefix("PUSH ") {
                        // Unsolicited server notification.
                        w.write_all(format!("{push}\n").as_bytes()).await.unwrap();
                    } else {
                        match cmd.as_str() {
                            "version" => {
                                w.write_all(b"version=3.13.8 platform=Linux\nerror id=0 msg=ok\n").await.unwrap();
                            }
                            "quit" => {
                                w.write_all(b"error id=0 msg=ok\n").await.unwrap();
                                break;
                            }
                            _ => {
                                w.write_all(b"error id=0 msg=ok\n").await.unwrap();
                            }
                        }
                    }
                }
                push = rx_in.recv() => {
                    match push {
                        Some(p) => w.write_all(format!("{p}\n").as_bytes()).await.unwrap(),
                        None => break,
                    }
                }
            }
        }
    });
    (port, tx_in)
}

#[tokio::test(flavor = "multi_thread")]
async fn notification_reaches_subscriber() {
    let (port, cmd_tx) = spawn_fake_server().await;
    let conn = QueryConnection::connect(QueryOptions {
        port,
        ..Default::default()
    })
    .await
    .expect("connect");

    let mut rx = conn.subscribe();
    // Subscription established BEFORE the notification is pushed.
    cmd_tx
        .send("notifytextmessage targetmode=3 msg=hello".into())
        .await
        .unwrap();

    let got = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await;
    match got {
        Ok(Ok(cmd)) => assert_eq!(cmd.name, "notifytextmessage"),
        other => panic!("no notification received: {other:?}"),
    }
}
