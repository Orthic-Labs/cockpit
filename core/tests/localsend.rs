//! The receiving side of nearby sharing, through a real TLS connection to a
//! service on this machine. Discovery is not exercised (multicast is not
//! available everywhere tests run).
#![cfg(feature = "localsend")]

use pulse_core::localsend::proto::{self, PrepareUploadResponse};
use pulse_core::localsend::{Config, Service, net};
use serde_json::json;
use std::io::Write;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

fn temp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("pulse-localsend-{name}-{}", proto::random_hex(4)))
}

fn start(port: u16, save: &Path, state: &Path) -> Arc<Service> {
    let config = Config {
        alias: "Test Mac (Pulse)".into(),
        port,
        save_dir: save.to_path_buf(),
        state_dir: state.to_path_buf(),
        device_model: "Mac".into(),
    };
    Arc::new(Service::start(config, Arc::new(|_| {})).expect("service starts"))
}

/// Answer the first request that arrives.
fn answer(service: Arc<Service>, accept: bool) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        for _ in 0..400 {
            if let Some(request) = service.snapshot().incoming.first() {
                assert!(service.respond(&request.id.clone(), accept));
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("no request arrived");
    })
}

fn prepare(port: u16, file_name: &str, size: usize) -> net::Reply {
    let body = json!({
        "info": {"alias": "Phone", "version": "2.0", "deviceType": "mobile",
                 "fingerprint": "phone-1", "port": 53317, "protocol": "https"},
        "files": {"f1": {"id": "f1", "fileName": file_name, "size": size, "fileType": "text/plain"}}
    })
    .to_string();
    let ip: IpAddr = "127.0.0.1".parse().unwrap();
    net::request_json(
        ip,
        port,
        true,
        "",
        "POST",
        "/api/localsend/v2/prepare-upload",
        Some(body.as_bytes()),
        Duration::from_secs(30),
    )
    .expect("prepare-upload answers")
}

fn upload(port: u16, session: &PrepareUploadResponse, token: &str, bytes: &[u8]) -> u16 {
    let ip: IpAddr = "127.0.0.1".parse().unwrap();
    let mut wire = net::connect(ip, port, true, "", Duration::from_secs(30)).unwrap();
    let target = format!(
        "/api/localsend/v2/upload?sessionId={}&fileId=f1&token={}",
        session.session_id, token
    );
    let reply = net::call(
        &mut wire,
        "POST",
        "127.0.0.1",
        &target,
        Some("application/octet-stream"),
        bytes.len() as u64,
        &mut |w| w.write_all(bytes),
    )
    .unwrap();
    wire.finish();
    reply.status
}

#[test]
fn accepted_files_are_saved_inside_the_save_folder_and_never_overwrite() {
    let (save, state) = (temp("save"), temp("state"));
    let service = start(53941, &save, &state);

    for expected in ["hello.txt", "hello (1).txt"] {
        let helper = answer(service.clone(), true);
        // A hostile name: the traversal must be dropped, not followed.
        let reply = prepare(53941, "../../evil/hello.txt", 5);
        helper.join().unwrap();
        assert_eq!(reply.status, 200);
        let session: PrepareUploadResponse = serde_json::from_slice(&reply.body).unwrap();
        let token = session.files["f1"].clone();
        assert_eq!(upload(53941, &session, "wrong-token", b"hello"), 403);
        assert_eq!(upload(53941, &session, &token, b"hello"), 200);
        // The test sender presents no certificate, so its files are kept apart.
        let saved = save.join("Received (unverified)").join("evil").join(expected);
        assert_eq!(std::fs::read(&saved).unwrap(), b"hello");
    }
    assert!(!save.parent().unwrap().join("evil").exists());
    let done = service.snapshot().transfers;
    assert!(done.iter().all(|t| t.state == "done"));
    let _ = std::fs::remove_dir_all(&save);
    let _ = std::fs::remove_dir_all(&state);
}

#[test]
fn declined_requests_get_403_and_save_nothing() {
    let (save, state) = (temp("save"), temp("state"));
    let service = start(53942, &save, &state);
    let helper = answer(service.clone(), false);
    let reply = prepare(53942, "photo.jpg", 3);
    helper.join().unwrap();
    assert_eq!(reply.status, 403);
    assert!(!save.join("photo.jpg").exists());
    let _ = std::fs::remove_dir_all(&state);
}
