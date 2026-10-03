//! Linux synthetic executable only. No official CLI, model/vendor or Broker.
#![cfg(target_os = "linux")]
use super::super::{
    provider_client::tests::request,
    provider_ready::{self, tests::Fixture},
};
use super::*;
use std::os::unix::fs::PermissionsExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn executable(fixture: &Fixture, stall: bool) -> (PathBuf, PathBuf) {
    let home = fixture.root.join("claude-home");
    std::fs::create_dir(&home).unwrap();
    let path = fixture.root.join("fake-claude");
    std::fs::write(&path,format!(r#"#!/usr/bin/python3
import http.client,os,sys,time,urllib.parse
assert sys.argv[1:]==['-p','--output-format','json','--no-session-persistence','--setting-sources','','--max-turns','1','--tools','','--strict-mcp-config','--mcp-config','{{"mcpServers":{{}}}}']
assert 'ANTHROPIC_API_KEY' not in os.environ
assert 'CHIMAERA_PROVIDER_CAPABILITY' not in os.environ
assert os.environ['CLAUDE_CONFIG_DIR'].startswith('/proc/self/fd/')
assert os.path.isdir(os.environ['CLAUDE_CONFIG_DIR'])
assert os.environ['CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC']=='1'
with open(os.environ['HOME']+'/started','w') as f:f.write(str(os.getpid()))
assert sys.stdin.buffer.read()==b'synthetic input'
with open(os.environ['HOME']+'/input-closed','w') as f:f.write('eof')
if {stall}:time.sleep(60)
u=urllib.parse.urlsplit(os.environ['ANTHROPIC_BASE_URL'])
assert u.hostname=='127.0.0.1'
c=http.client.HTTPConnection(u.hostname,u.port,timeout=5)
c.request('POST','/v1/messages?beta=true',body=b'{{}}',headers={{'Content-Type':'application/json','Authorization':'Bearer '+os.environ['CLAUDE_CODE_OAUTH_TOKEN']}})
r=c.getresponse();assert r.status==200
data=r.read(131073);assert len(data)<=131072
sys.stdout.buffer.write(data)
"#,stall=if stall{"True"}else{"False"})).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    (path, home)
}
async fn answer(peer: &mut tokio::net::UnixStream, req: &wire::Request) {
    loop {
        let mut h = [0; 5];
        peer.read_exact(&mut h).await.unwrap();
        let h = wire::FrameHeader::decode(&h).unwrap();
        let mut bytes = vec![0; h.length];
        peer.read_exact(&mut bytes).await.unwrap();
        if h.kind == wire::FrameKind::RequestEnd {
            break;
        }
        assert_eq!(h.kind, wire::FrameKind::RequestData);
    }
    let head = wire::Response {
        version: 1,
        binding: req.binding.clone(),
        request_id: req.request_id.clone(),
        result: wire::Reply::ClaudeHead {
            head: wire::ClaudeHead {
                status: 200,
                headers: wire::Headers {
                    content_type: wire::ContentType::Json,
                    retry_after_seconds: None,
                },
            },
        },
    };
    let body = b"{\"synthetic\":true}";
    let end = wire::StreamEnd {
        version: 1,
        binding: req.binding.clone(),
        request_id: req.request_id.clone(),
        bytes: body.len() as u64,
    };
    for (kind, bytes) in [
        (3, wire::encode_control(&head).unwrap()),
        (4, Zeroizing::new(body.to_vec())),
        (5, wire::encode_control(&end).unwrap()),
    ] {
        let mut h = [kind, 0, 0, 0, 0];
        h[1..].copy_from_slice(&(bytes.len() as u32).to_be_bytes());
        peer.write_all(&h).await.unwrap();
        peer.write_all(&bytes).await.unwrap();
    }
    peer.shutdown().await.unwrap();
}
#[tokio::test]
async fn synthetic_child_uses_frontend_only_and_closes_group_before_activity_settles() {
    let fixture = Fixture::new(Duration::from_secs(1));
    fixture.verified().await;
    let (executable, home) = executable(&fixture, false);
    let state = fixture.state.clone();
    let captured = home.clone();
    let task = tokio::spawn(async move {
        exercise_at(
            &state,
            Zeroizing::new(b"synthetic input".to_vec()),
            executable,
            captured,
            Duration::from_secs(3),
        )
        .await
    });
    let (mut peer, req) = request(&fixture.listener).await;
    assert_eq!(std::fs::read(home.join("input-closed")).unwrap(), b"eof");
    assert!(matches!(req.command, wire::Command::ClaudeStream { .. }));
    answer(&mut peer, &req).await;
    let result = tokio::time::timeout(Duration::from_secs(8), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(&*result, b"{\"synthetic\":true}");
    assert_eq!(provider_ready::active(&fixture.state), 0);
    assert!(!home.join(".claude/.credentials.json").exists());
    fixture.finish().await;
}
#[tokio::test]
async fn lost_observer_and_child_deadline_reap_stalled_real_child_without_provider_effect() {
    for cancel in [true, false] {
        let fixture = Fixture::new(Duration::from_secs(1));
        fixture.verified().await;
        let (executable, home) = executable(&fixture, true);
        let state = fixture.state.clone();
        let captured = home.clone();
        let task = tokio::spawn(async move {
            exercise_at(
                &state,
                Zeroizing::new(b"synthetic input".to_vec()),
                executable,
                captured,
                Duration::from_millis(if cancel { 1500 } else { 250 }),
            )
            .await
        });
        let pid: i32 = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(value) = tokio::fs::read_to_string(home.join("started")).await {
                    if let Ok(pid) = value.parse() {
                        break pid;
                    }
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(provider_ready::active(&fixture.state), 1);
        if cancel {
            task.abort();
        }
        let result = task.await;
        assert!(result.is_err() || result.unwrap().is_err());
        tokio::time::timeout(Duration::from_secs(7), async {
            while provider_ready::active(&fixture.state) != 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(unsafe { nix::libc::kill(pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(nix::libc::ESRCH)
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(30), fixture.listener.accept())
                .await
                .is_err()
        );
        fixture.finish().await;
    }
}
#[tokio::test]
async fn config_capture_refuses_symlink_and_keeps_original_directory() {
    use std::os::unix::fs::MetadataExt;
    let fixture = Fixture::new(Duration::from_secs(1));
    let home = fixture.root.join("home");
    std::fs::create_dir(&home).unwrap();
    let config = Config::capture(&home).unwrap();
    let identity = config.0.metadata().unwrap().ino();
    std::fs::rename(home.join(".claude"), home.join("original")).unwrap();
    std::fs::create_dir(home.join(".claude")).unwrap();
    assert_eq!(config.0.metadata().unwrap().ino(), identity);
    assert_ne!(
        std::fs::metadata(home.join(".claude")).unwrap().ino(),
        identity
    );
    std::fs::remove_dir(home.join(".claude")).unwrap();
    std::os::unix::fs::symlink(home.join("original"), home.join(".claude")).unwrap();
    assert!(matches!(
        Config::capture(&home),
        Err(wire::Error::StateChanged)
    ));
}
