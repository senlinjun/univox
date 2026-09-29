//! Manual protocol probe: boots the server and speaks raw ServerQuery.
//! Used to inspect real server responses while developing; not an
//! assertion test suite (all tests "pass" unless the probe panics).

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use test_support::Ts3Server;

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn raw_probe() {
    let server = Ts3Server::start().await.expect("boot");
    let stream = tokio::net::TcpStream::connect(("127.0.0.1", server.query_port))
        .await
        .expect("connect");
    let (r, mut w) = stream.into_split();
    let mut reader = BufReader::new(r);
    let mut junk = Vec::new();
    // Greeting: two lines.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), reader.read_until(b'\n', &mut junk)).await;
    junk.clear();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), reader.read_until(b'\n', &mut junk)).await;
    junk.clear();

    w.write_all(format!("login serveradmin {}\n", server.serveradmin_password).as_bytes())
        .await
        .unwrap();
    drain_until_error(&mut reader).await;
    w.write_all(b"use 1\n").await.unwrap();
    drain_until_error(&mut reader).await;

    // Register on connection 1 for all event families.
    for c in [
        "instanceedit serverinstance_serverquery_flood_commands=9999 serverinstance_serverquery_flood_time=1",
        "servernotifyregister event=server",
        "servernotifyregister event=channel id=0",
        "servernotifyregister event=textserver",
    ] {
        println!("== REG: {c}");
        w.write_all(format!("{c}\n").as_bytes()).await.unwrap();
        drain_until_error(&mut reader).await;
    }

    // Connection 2: create a channel, move it, delete it.
    let stream2 = tokio::net::TcpStream::connect(("127.0.0.1", server.query_port))
        .await
        .unwrap();
    let (r2, mut w2) = stream2.into_split();
    let mut reader2 = BufReader::new(r2);
    let mut junk2 = Vec::new();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), reader2.read_until(b'\n', &mut junk2)).await;
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), reader2.read_until(b'\n', &mut junk2)).await;
    w2.write_all(format!("login serveradmin {}\n", server.serveradmin_password).as_bytes()).await.unwrap();
    drain_until_error(&mut reader2).await;
    w2.write_all(b"use 1\n").await.unwrap();
    drain_until_error(&mut reader2).await;
    for c in [
        "channelcreate channel_name=NotifProbe cpid=0 channel_flag_permanent=1",
        "channeldelete cid=2 force=1",
        "channelcreate channel_name=NotifProbe cpid=0 channel_flag_permanent=1",
        "sendtextmessage targetmode=3 target=0 msg=ping",
    ] {
        println!("== B CMD: {c}");
        w2.write_all(format!("{c}\n").as_bytes()).await.unwrap();
        drain_until_error(&mut reader2).await;
    }

    // Print everything A receives for 2 seconds.
    println!("== A notifications:");
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        let mut line = String::new();
        match tokio::time::timeout(deadline.saturating_duration_since(tokio::time::Instant::now()), reader.read_line(&mut line)).await {
            Ok(Ok(n)) if n > 0 => println!("  A LINE: {}", line.trim()),
            _ => break,
        }
    }
}

async fn drain_until_error(reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        let mut line = String::new();
        match tokio::time::timeout(
            deadline.saturating_duration_since(tokio::time::Instant::now()),
            reader.read_line(&mut line),
        )
        .await
        {
            Err(_) => {
                println!("  [timeout]");
                break;
            }
            Ok(Err(_)) | Ok(Ok(0)) => {
                println!("  [eof]");
                break;
            }
            Ok(Ok(_)) => {}
        }
        let t = line.trim();
        println!("  LINE: {t}");
        if t.starts_with("error") {
            break;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn raw_probe_querylogin_use0() {
    let server = Ts3Server::start().await.expect("boot");
    let stream = tokio::net::TcpStream::connect(("127.0.0.1", server.query_port))
        .await
        .expect("connect");
    let (r, mut w) = stream.into_split();
    let mut reader = BufReader::new(r);
    let mut junk = Vec::new();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), reader.read_until(b'\n', &mut junk)).await;
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), reader.read_until(b'\n', &mut junk)).await;

    w.write_all(format!("login serveradmin {}\n", server.serveradmin_password).as_bytes()).await.unwrap();
    drain_until_error(&mut reader).await;
    for c in [
        "use 0",
        "queryloginadd client_login_name=probequery client_login_password=pw12345",
        "queryloginlist",
        "channelclientlist cid=1",
    ] {
        println!("== CMD: {c}");
        w.write_all(format!("{c}\n").as_bytes()).await.unwrap();
        drain_until_error(&mut reader).await;
    }
}
