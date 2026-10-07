//! Voice pipeline acceptance (FEATURES.md §6): two sessions in the default
//! channel; A sends a 440 Hz sine, B receives, decodes and mixes it.
#![cfg(feature = "voice")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use test_support::Ts3Server;
use univox_core::audio::{AudioPacket, AudioSink};
use univox_core::event::Event;
use univox_core::session::Session;
use univox_core::{ConnectOptions, Credential, InitialState};
use univox_ts3::ext::Ts3Ext;
use univox_ts3::Ts3Session;
use univox_ts3_proto::Identity;
use univox_voice::SineSource;

/// Collects every mixed output frame.
struct CollectSink {
    packets: Arc<Mutex<Vec<Vec<f32>>>>,
}

#[async_trait]
impl AudioSink for CollectSink {
    async fn write(&mut self, packet: AudioPacket) -> univox_core::error::Result<()> {
        self.packets.lock().unwrap().push(packet.samples);
        Ok(())
    }
}

async fn connect_voice_session(server: &Ts3Server, nickname: &str) -> Arc<Ts3Session> {
    let mut opts = ConnectOptions::new(format!("127.0.0.1:{}", server.voice_port))
        .nickname(nickname)
        .credential(Credential::Anonymous);
    // Make sure the mic is not flagged muted for the voice path.
    opts.initial_state = InitialState::default();
    Ts3Session::connect(opts, Identity::create())
        .await
        .expect("connect")
}

#[tokio::test(flavor = "multi_thread")]
async fn sine_roundtrip_between_two_sessions() {
    let server = Ts3Server::start().await.expect("boot");
    let sender = connect_voice_session(&server, "Voice Sender").await;
    let listener = connect_voice_session(&server, "Voice Listener").await;

    // Subscribe the listener to the default channel so the server relays
    // voice (ignored when the server auto-subscribes).
    let _ = listener
        .book()
        .with(|b| {
            b.channels
                .keys()
                .next()
                .map(|cid| cid.as_u64().unwrap_or(0))
        })
        .flatten();

    let collected = Arc::new(Mutex::new(Vec::new()));
    listener
        .start_receiving(Box::new(CollectSink {
            packets: collected.clone(),
        }))
        .await
        .expect("start_receiving");

    tokio::time::sleep(Duration::from_millis(300)).await;

    sender
        .start_sending(Box::new(SineSource::new(440.0)))
        .await
        .expect("start_sending");

    // Send for ~1.2 s (≈60 frames), then stop and drain.
    tokio::time::sleep(Duration::from_millis(1200)).await;
    sender.stop_sending().await.expect("stop_sending");
    tokio::time::sleep(Duration::from_millis(400)).await;

    let packets = collected.lock().unwrap().clone();
    let voice_frames: Vec<&Vec<f32>> = packets
        .iter()
        .filter(|p| p.iter().any(|s| s.abs() > 0.05))
        .collect();
    assert!(
        voice_frames.len() >= 30,
        "expected >=30 audible frames, got {} of {}",
        voice_frames.len(),
        packets.len()
    );

    // All frames are canonical 20 ms @ 48 kHz.
    for f in packets.iter().take(10) {
        assert_eq!(f.len(), univox_voice::FRAME_SAMPLES);
    }

    // Frequency sanity: 440 Hz over 20 ms ≈ 8.8 rising zero crossings.
    let crossing_counts: Vec<f32> = voice_frames
        .iter()
        .map(|f| {
            f.windows(2)
                .filter(|w| w[0] < 0.0 && w[1] >= 0.0)
                .count() as f32
        })
        .collect();
    let avg = crossing_counts.iter().sum::<f32>() / crossing_counts.len() as f32;
    assert!(
        (6.0..=12.0).contains(&avg),
        "unexpected dominant frequency: {avg} crossings/frame (want ~8.8)"
    );

    listener.stop_receiving().await.expect("stop_receiving");
    sender.disconnect(None).await.ok();
    listener.disconnect(None).await.ok();
}

/// A whispers to the channel; B hears it outside the normal voice path.
#[tokio::test(flavor = "multi_thread")]
async fn whisper_reaches_target() {
    let server = Ts3Server::start().await.expect("boot");
    let whisperer = connect_voice_session(&server, "Whisperer").await;
    let listener = connect_voice_session(&server, "Eavesdropper").await;

    let collected = Arc::new(Mutex::new(Vec::new()));
    listener
        .start_receiving(Box::new(CollectSink { packets: collected.clone() }))
        .await
        .expect("start_receiving");
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Encode whisper frames locally (20 ms of sine each) and send 40 of them.
    let codec = univox_voice::OpusEncoder::new().expect("encoder");
    let mut source = SineSource::new(440.0);
    for _ in 0..40 {
        let frame = source.next_frame();
        let packet = codec.encode(&frame).expect("encode");
        whisperer
            .send_whisper_to_channel(&univox_core::id::ChannelId::from_u64(1), &packet)
            .await
            .expect("whisper send");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(400)).await;

    let packets = collected.lock().unwrap().clone();
    let audible = packets.iter().filter(|p| p.iter().any(|x| x.abs() > 0.05)).count();
    assert!(audible >= 20, "whisper not audible at target: {audible} frames");

    whisperer.disconnect(None).await.ok();
    listener.disconnect(None).await.ok();
}

/// A whispers to a member list (`send_whisper`, legacy multi-target
/// format); B hears it and the speaking events flag it as a whisper
/// (FEATURES.md §6.4).
#[tokio::test(flavor = "multi_thread")]
async fn member_whisper_reaches_target_and_flags_whispering() {
    use univox_core::id::MemberId;

    let server = Ts3Server::start().await.expect("boot");
    let whisperer = connect_voice_session(&server, "Member Whisperer").await;
    let listener = connect_voice_session(&server, "Whisper Listener").await;

    // The listener's own clid is the whisper destination.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let target = loop {
        let t = listener
            .book()
            .with(|b| b.self_member.member_id.clone())
            .flatten();
        if let Some(t) = t {
            break t;
        }
        assert!(tokio::time::Instant::now() < deadline, "listener clid unknown");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };

    let mut events = listener.events();
    let collected = Arc::new(Mutex::new(Vec::new()));
    listener
        .start_receiving(Box::new(CollectSink { packets: collected.clone() }))
        .await
        .expect("start_receiving");
    tokio::time::sleep(Duration::from_millis(300)).await;

    let codec = univox_voice::OpusEncoder::new().expect("encoder");
    let mut source = SineSource::new(440.0);
    for _ in 0..40 {
        let frame = source.next_frame();
        let packet = codec.encode(&frame).expect("encode");
        whisperer
            .send_whisper(
                &[univox_ts3::WhisperTarget::Member(MemberId::from_u64(
                    target.as_u64().unwrap_or(0),
                ))],
                &packet,
            )
            .await
            .expect("send_whisper");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(400)).await;

    // Audio arrived through the whisper path…
    let packets = collected.lock().unwrap().clone();
    let audible = packets.iter().filter(|p| p.iter().any(|x| x.abs() > 0.05)).count();
    assert!(audible >= 20, "member whisper not audible: {audible} frames");

    // …and the speaking events flagged it as a whisper.
    let mut saw_whisper_start = false;
    while let Ok(Some(ev)) =
        tokio::time::timeout(Duration::from_millis(500), events.next()).await
    {
        if matches!(ev.as_ref(), Event::SpeakingStarted { whispering: true, .. }) {
            saw_whisper_start = true;
            break;
        }
    }
    assert!(saw_whisper_start, "no whisper-flagged SpeakingStarted");

    whisperer.disconnect(None).await.ok();
    listener.disconnect(None).await.ok();
}
