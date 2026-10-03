//! Real fake-gh subprocess/loopback HTTP; synthetic token and protection only.
use super::super::{
    provider_client::tests::{reply, request},
    provider_ready::{self, tests::Fixture},
};
use super::*;
use std::os::unix::fs::PermissionsExt;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

fn executable(fixture: &Fixture, port: u16) -> PathBuf {
    let home = fixture.root.join("gh-home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(home.join(".config/gh")).unwrap();
    std::fs::write(
        home.join(".config/gh/config.yml"),
        "api_host: attacker.invalid\nhttp_unix_socket: /tmp/attacker.sock\n",
    )
    .unwrap();
    let path = fixture.root.join("fake-gh");
    // Test-only endpoint is embedded in the fake executable, never configurable
    // in the production viewer or its token/environment construction.
    let script = format!(
        r#"#!/usr/bin/python3
import http.client,json,os,sys
assert sys.argv[1:]==['api','--hostname','github.com','--method','GET','user']
assert 'GITHUB_TOKEN' not in os.environ
assert 'CHIMAERA_PROVIDER_CAPABILITY' not in os.environ
home=os.environ['HOME']
config=os.environ['GH_CONFIG_DIR']
assert config.startswith('/proc/self/fd/') or config.startswith('/dev/fd/')
assert not os.path.exists(config+'/config.yml')
try:
 f=open(config+'/config.yml','w');f.close();raise RuntimeError('mutable gh config')
except OSError:pass
assert not os.path.exists(home+'/.config/gh/hosts.yml')
with open(home+'/started','w') as f:f.write(str(os.getpid()))
c=http.client.HTTPConnection('127.0.0.1',{port},timeout=20)
c.request('GET','/user',headers={{'Authorization':'token '+os.environ['GH_TOKEN']}})
r=c.getresponse();data=r.read(131073)
if r.status!=200:sys.exit(7)
sys.stdout.buffer.write(data)
"#
    );
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    path
}
async fn http(listener: &TcpListener) -> (tokio::net::TcpStream, String) {
    tokio::time::timeout(Duration::from_secs(2), async {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::with_capacity(4096);
        while !bytes.ends_with(b"\r\n\r\n") {
            assert!(bytes.len() < 4096);
            let mut byte = [0];
            stream.read_exact(&mut byte).await.unwrap();
            bytes.push(byte[0]);
        }
        (stream, String::from_utf8(bytes).unwrap())
    })
    .await
    .unwrap()
}
async fn response(stream: &mut tokio::net::TcpStream, success: bool) {
    let reply = if success {
        b"HTTP/1.1 200 OK\r\nContent-Length: 17\r\nConnection: close\r\n\r\n{\"login\":\"alice\"}"
            .as_slice()
    } else {
        b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".as_slice()
    };
    stream.write_all(reply).await.unwrap();
    stream.shutdown().await.unwrap();
}
#[tokio::test]
async fn real_child_uses_new_access_each_invocation_without_replaying_rejection() {
    let fixture = Fixture::new(Duration::from_secs(1));
    fixture.verified().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let executable = executable(&fixture, listener.local_addr().unwrap().port());
    for (token, success) in [
        ("synthetic-A", true),
        ("synthetic-B", true),
        ("synthetic-old", false),
    ] {
        let state = fixture.state.clone();
        let executable = executable.clone();
        let home = fixture.root.join("gh-home");
        let task = tokio::spawn(async move {
            viewer_at(&state, executable, home, Duration::from_secs(3)).await
        });
        let (mut peer, req) = request(&fixture.listener).await;
        assert!(matches!(req.command, wire::Command::GithubGhAccess {}));
        reply(&mut peer, &req, token, false).await;
        let (mut stream, headers) = http(&listener).await;
        assert!(headers.contains(&format!("Authorization: token {token}\r\n")));
        response(&mut stream, success).await;
        let result = tokio::time::timeout(Duration::from_secs(6), task)
            .await
            .unwrap()
            .unwrap();
        if success {
            assert_eq!(&*result.unwrap(), b"{\"login\":\"alice\"}");
        } else {
            assert!(matches!(result, Err(wire::Error::Unavailable)));
        }
        assert_eq!(provider_ready::active(&fixture.state), 0);
        assert!(
            tokio::time::timeout(Duration::from_millis(30), listener.accept())
                .await
                .is_err()
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(30), fixture.listener.accept())
                .await
                .is_err()
        );
        assert!(!fixture.root.join("gh-home/.config/gh/hosts.yml").exists());
    }
    fixture.finish().await;
}
#[tokio::test]
async fn lost_observer_keeps_real_child_counted_until_configuration_cleanup() {
    let fixture = Fixture::new(Duration::from_secs(1));
    fixture.verified().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let executable = executable(&fixture, listener.local_addr().unwrap().port());
    let state = fixture.state.clone();
    let home = fixture.root.join("gh-home");
    let task =
        tokio::spawn(
            async move { viewer_at(&state, executable, home, Duration::from_secs(20)).await },
        );
    let (mut peer, req) = request(&fixture.listener).await;
    reply(&mut peer, &req, "synthetic-A", false).await;
    let (_stream, _) = http(&listener).await;
    let pid: i32 = tokio::fs::read_to_string(fixture.root.join("gh-home/started"))
        .await
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(provider_ready::active(&fixture.state), 1);
    task.abort();
    let _ = task.await;
    tokio::time::timeout(Duration::from_secs(6), async {
        while provider_ready::active(&fixture.state) != 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(unsafe { nix::libc::kill(pid, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(nix::libc::ESRCH)
    );
    fixture.finish().await;
}
#[tokio::test]
async fn https_helper_refuses_selectors_before_broker_and_delivers_exact_credentials() {
    let fixture = Fixture::new(Duration::from_secs(1));
    fixture.verified().await;
    for input in [
        b"protocol=http\nhost=github.com\n\n".as_slice(),
        b"protocol=https\nhost=elsewhere.test\n\n",
        b"protocol=https\nhost=github.com\npath=user\n\n",
        b"protocol=https\nhost=github.com\nhost=github.com\n\n",
        b"protocol=https\nhost=github.com\n\nextra",
    ] {
        let (write, _read) = tokio::io::duplex(4096);
        assert!(matches!(
            credentials(&fixture.state, input, write).await,
            Err(wire::Error::InvalidRequest)
        ));
    }
    assert_eq!(provider_ready::active(&fixture.state), 0);
    assert!(
        tokio::time::timeout(Duration::from_millis(30), fixture.listener.accept())
            .await
            .is_err()
    );
    let state = fixture.state.clone();
    let (write, read) = tokio::io::duplex(4096);
    let task = tokio::spawn(async move {
        credentials(&state, b"protocol=https\nhost=github.com\n\n", write).await
    });
    let (mut peer, req) = request(&fixture.listener).await;
    assert!(matches!(
        req.command,
        wire::Command::GithubHttpsCredentials { .. }
    ));
    reply(&mut peer, &req, "synthetic-A", false).await;
    let mut bytes = Zeroizing::new(Vec::with_capacity(4096));
    tokio::time::timeout(
        Duration::from_secs(2),
        read.take(4096).read_to_end(&mut bytes),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        &*bytes,
        b"username=x-access-token\npassword=synthetic-A\n\n"
    );
    task.await.unwrap().unwrap();
    fixture.finish().await;
}

#[tokio::test]
async fn lost_observer_before_reply_cannot_deliver_credentials() {
    let fixture = Fixture::new(Duration::from_secs(1));
    fixture.verified().await;
    let state = fixture.state.clone();
    let (write, mut read) = tokio::io::duplex(4096);
    let task = tokio::spawn(async move {
        credentials(&state, b"protocol=https\nhost=github.com\n\n", write).await
    });
    let (mut peer, _) = request(&fixture.listener).await;
    task.abort();
    let _ = task.await;
    tokio::time::timeout(Duration::from_secs(2), async {
        let mut byte = [0];
        assert_eq!(peer.read(&mut byte).await.unwrap(), 0);
        assert_eq!(read.read(&mut byte).await.unwrap(), 0);
        while provider_ready::active(&fixture.state) != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    fixture.finish().await;
}

// CI downloads the exact official Debian binary (same digest as the worker
// image) and explicitly runs this ignored test. No vendor API is requested.
#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires the CI checksum-verified official gh2.102.0 binary"]
async fn pinned_gh_2102_ignores_project_endpoint_config_through_unlinked_directory() {
    let executable = PathBuf::from(
        std::env::var_os("CHIMAERA_TEST_GH_2102").expect("pinned fixture binary required"),
    );
    let fixture = Fixture::new(Duration::from_secs(1));
    let home = fixture.root.join("gh-home");
    let hostile = home.join(".config/gh");
    std::fs::create_dir_all(&hostile).unwrap();
    std::fs::write(
        hostile.join("config.yml"),
        "api_host: attacker.invalid\nhttp_unix_socket: /tmp/attacker.sock\n",
    )
    .unwrap();
    let config = EmptyConfig::new().unwrap();
    assert!(std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(format!("{}/config.yml", config.path()))
        .is_err());
    async fn invoke(
        executable: &Path,
        home: &Path,
        args: &[&str],
        config: Option<&EmptyConfig>,
    ) -> (bool, Zeroizing<Vec<u8>>, Zeroizing<Vec<u8>>) {
        let mut command = tokio::process::Command::new(executable);
        command
            .args(args)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", home)
            .env("GH_TOKEN", "synthetic-no-vendor")
            .env("GH_NO_UPDATE_NOTIFIER", "1")
            .env("GH_NO_EXTENSION_UPDATE_NOTIFIER", "1")
            .env("GH_TELEMETRY", "false")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(config) = config {
            command.env("GH_CONFIG_DIR", config.path());
            config.inherit(&mut command);
        } else {
            command.env("GH_CONFIG_DIR", home.join(".config/gh"));
        }
        let mut child = Child::spawn(&mut command).unwrap();
        let pid = child.child.id();
        let output = child.child.stdout.take().unwrap();
        let error = child.child.stderr.take().unwrap();
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::try_join!(bounded(output), bounded(error), async {
                child.wait().await.map_err(|_| wire::Error::Unavailable)
            })
        })
        .await;
        child
            .terminate(pid)
            .await
            .expect("pinned config child cleanup required");
        let (output, error, status) = result.expect("bounded pinned config process").unwrap();
        (status.success(), output, error)
    }
    let (ok, version, _) = invoke(&executable, &home, &["--version"], Some(&config)).await;
    assert!(ok && version.starts_with(b"gh version 2.102.0 "));
    for (key, hostile_value) in [
        ("api_host", "attacker.invalid"),
        ("http_unix_socket", "/tmp/attacker.sock"),
    ] {
        let args = ["config", "get", key, "--host", "github.com"];
        let (ok, baseline, _) = invoke(&executable, &home, &args, None).await;
        assert!(ok && std::str::from_utf8(&baseline).unwrap().trim() == hostile_value);
        let (ok, empty, error) = invoke(&executable, &home, &args, Some(&config)).await;
        assert!(empty.iter().all(u8::is_ascii_whitespace));
        assert!(ok || error.as_slice() == format!("could not find key \"{key}\"\n").as_bytes());
    }
    // Read-only config calls leave the hostile project files intact and cannot
    // create any new entry beneath the unnamed fd-backed configuration root.
    assert!(std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(format!("{}/hosts.yml", config.path()))
        .is_err());
}
