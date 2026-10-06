//! File transfer integration tests (FEATURES.md §11): channel file
//! upload/download roundtrip and the avatar path.

use std::sync::Arc;

use test_support::Ts3Server;
use univox_core::id::{ChannelId, DbId};
use univox_core::session::Session;
use univox_core::{ConnectOptions, Credential};
use univox_ts3::ext::Ts3Ext;
use univox_ts3::Ts3Session;
use univox_ts3_proto::{Identity, RowExt};

async fn spawn_admin(nickname: &str, server: &Ts3Server) -> Arc<Ts3Session> {
    let opts = ConnectOptions::new(format!("127.0.0.1:{}", server.voice_port))
        .nickname(nickname)
        .credential(Credential::Anonymous);
    let session = Ts3Session::connect(opts, Identity::create())
        .await
        .expect("connect");
    session
        .use_privilege_key(&server.admin_token)
        .await
        .expect("token");
    session
}

#[tokio::test(flavor = "multi_thread")]
async fn file_transfer_roundtrip() {
    let server = Ts3Server::start().await.expect("boot");
    let session = spawn_admin("Filer", &server).await;
    let channel = ChannelId::from_u64(1);
    let payload: Vec<u8> = (0..50_000u32).map(|i| (i % 251) as u8).collect();

    eprintln!("UPLOAD begin");
    let up = session.upload_file(&channel, "/univox_test.bin", &payload, true).await;
    eprintln!("UPLOAD result: {up:?}");
    up.expect("upload");

    // The file shows up in the listing.
    let listing = session.list_files(&channel, "/", None).await.expect("ftgetfilelist");
    // The server strips the leading slash in the listing.
    assert!(
        listing
            .iter()
            .any(|r| r.get("name").map(|n| n.trim_start_matches('/')) == Some("univox_test.bin")),
        "uploaded file missing from listing: {listing:?}"
    );

    let downloaded = session
        .download_file(&channel, "/univox_test.bin")
        .await
        .expect("download");
    assert_eq!(downloaded.len(), payload.len(), "size mismatch");
    assert_eq!(downloaded, payload, "content mismatch");

    session
        .delete_file(&channel, "/univox_test.bin", None)
        .await
        .expect("ftdeletefile");
    let listing = session.list_files(&channel, "/", None).await.expect("ftgetfilelist");
    assert!(
        listing
            .iter()
            .all(|r| r.get("name").map(|n| n.trim_start_matches('/')) != Some("univox_test.bin")),
        "file not deleted"
    );

    session.disconnect(None).await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn avatar_upload_and_download() {
    let server = Ts3Server::start().await.expect("boot");
    let session = spawn_admin("Avatar Bot", &server).await;

    // Own database id.
    let me = univox_core::id::MemberId::from_u64(univox_ts3::self_clid(&session));
    let uid = session.uid_from_clid(&me).await.expect("uid");
    let dbid = session
        .dbid_from_uid(&uid)
        .await
        .expect("dbid_from_uid")
        .expect("dbid");

    // No avatar yet.
    let before = session.download_avatar(&dbid).await.expect("avatar before");
    assert!(before.is_none(), "unexpected pre-existing avatar");

    let png: Vec<u8> = vec![0x89, b'P', b'N', b'G', 0, 1, 2, 3, 4, 5, 6, 7, 8, 9];
    session.upload_avatar(&png).await.expect("avatar upload");

    // How did the server store it? (the uid may contain path separators)
    let root = session.list_files(&ChannelId::from_u64(0), "/", None).await.expect("root listing");
    println!("ROOT: {root:?}");

    let downloaded = session.download_avatar(&dbid).await.expect("avatar after");
    assert_eq!(downloaded.as_deref(), Some(png.as_slice()), "avatar bytes differ");

    let _ = DbId::from_u64(0); // keep the import honest
    session.disconnect(None).await.ok();
}
