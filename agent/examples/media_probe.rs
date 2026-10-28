//! Headless run of the native viewer against a live agent, for end-to-end media checks:
//! connects through the server, authenticates, then reports codec, encoder, decoded frames
//! and decoded audio level once a second.
//!
//!   AEGIRON_URL=http://127.0.0.1:21190 AEGIRON_EMAIL=… AEGIRON_PASSWORD=… \
//!   cargo run --example media_probe -- <device-id> <device-password> [seconds]
//!
//! AEGIRON_AUDIO_OUT=0 decodes audio without playing it (when the agent runs on this machine).

use std::sync::Arc;
use std::time::Duration;

use aegiron_agent::account::{self, Login};
use aegiron_agent::viewer::{Phase, Target, Viewer};

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into())).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (id, pw) = (args.first().expect("device id").clone(), args.get(1).expect("device password").clone());
    let secs: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(8);
    let server = std::env::var("AEGIRON_URL").unwrap_or_else(|_| "http://127.0.0.1:21114".into());
    let email = std::env::var("AEGIRON_EMAIL").unwrap_or_else(|_| "admin@aegiron.local".into());
    let password = std::env::var("AEGIRON_PASSWORD")?;
    let Login::Ok(session) = account::login(&server, true, &email, &password, "")? else { anyhow::bail!("2FA not supported here") };

    let target = Target { server, token: session.token, insecure: true, device_id: id, name: "probe".into(), direct: true, only: None };
    let v = Viewer::connect(target, Arc::new(|| {}));
    let mut sent = false;
    let mut connected_for = 0;
    let (mut audio_seconds, mut max_level, mut frames) = (0, 0f32, 0u32);
    for _ in 0..(secs + 30) {
        std::thread::sleep(Duration::from_secs(1));
        let st = v.state.lock().unwrap();
        match st.phase {
            Phase::Password if !sent => {
                drop(st);
                v.submit_password(&pw);
                sent = true;
                continue;
            }
            Phase::Closed => anyhow::bail!("closed: {:?}", st.error),
            Phase::Connected => {}
            _ => continue,
        }
        connected_for += 1;
        frames += st.fps;
        if connected_for % 2 == 0 {
            // A static screen sends nothing; ask for keyframes so decoding is exercised.
            v.send(serde_json::json!({"t": "refresh"}));
        }
        if let Some(l) = st.audio_level {
            max_level = max_level.max(l);
            if l > 0.001 {
                audio_seconds += 1;
            }
        }
        println!(
            "t={connected_for}s codec={} encoder={} audio={} fps={} kbps={} screen={}x{} audio_level={:?}",
            st.codec,
            st.encoder,
            st.info.as_ref().is_some_and(|i| i.audio),
            st.fps,
            st.kbps,
            st.fb.w,
            st.fb.h,
            st.audio_level
        );
        if connected_for >= secs {
            break;
        }
    }
    v.close();
    std::thread::sleep(Duration::from_millis(300));
    println!("SUMMARY frames_decoded={frames} audio_seconds_nonsilent={audio_seconds} max_audio_rms={max_level:.4}");
    Ok(())
}
