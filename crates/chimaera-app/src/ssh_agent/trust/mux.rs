//! The private foreground `-N -M` probe is authenticated before OpenSSH starts
//! its mux listener. Its bounded v4 alive receipt must name that owned child.
//! Protocol: openssh-portable V_10_3_P1/PROTOCOL.mux sections 1 and 4.
use super::super::selection::SelectionFailure;
use std::{
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::Path,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
};

fn refused() -> SelectionFailure {
    SelectionFailure::Unavailable
}
/// OpenSSH tries mux before its ordinary connection. If the private master
/// disappears, the fixed failed proxy prevents fresh network/authentication;
/// BatchMode by itself would still permit a new public-key login.
pub(super) fn forward_proxy(
    path: &Path,
    upstream: &chimaera_link::SshAuthDestination,
    destination: &chimaera_link::SshAuthDestination,
) -> Result<String, SelectionFailure> {
    upstream.validate().map_err(|_| refused())?;
    destination.validate().map_err(|_| refused())?;
    let path = path
        .to_str()
        .filter(|path| {
            path.starts_with('/')
                && path.len() <= 1024
                && !path.chars().any(|value| value.is_control() || value == '%')
        })
        .ok_or_else(refused)?;
    fn quote(value: &str) -> String {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
    let host = if destination.hostname.contains(':') {
        format!("[{}]", destination.hostname)
    } else {
        destination.hostname.clone()
    };
    Ok(format!(
        "/usr/bin/ssh -F /dev/null -S {} -o ControlMaster=no -o ControlPersist=no -o BatchMode=yes -o ProxyCommand=/usr/bin/false -o ProxyJump=none -o PubkeyAuthentication=no -o PasswordAuthentication=no -o KbdInteractiveAuthentication=no -o GSSAPIAuthentication=no -o HostbasedAuthentication=no -o IdentityAgent=none -o IdentityFile=none -o CertificateFile=none -o ForwardAgent=no -W {} -l {} -p {} -- {}",
        quote(path), quote(&format!("{host}:{}", destination.port)), quote(&upstream.user), upstream.port, quote(&upstream.hostname),
    ))
}
async fn frame(stream: &mut UnixStream) -> Result<Vec<u8>, SelectionFailure> {
    let size = stream.read_u32().await.map_err(|_| refused())? as usize;
    if !(8..=4096).contains(&size) {
        return Err(refused());
    }
    let mut bytes = vec![0; size];
    stream.read_exact(&mut bytes).await.map_err(|_| refused())?;
    Ok(bytes)
}
fn words(values: &[u32]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_be_bytes())
        .collect()
}
async fn send(stream: &mut UnixStream, values: &[u32]) -> Result<(), SelectionFailure> {
    let bytes = words(values);
    stream
        .write_u32(bytes.len() as u32)
        .await
        .map_err(|_| refused())?;
    stream.write_all(&bytes).await.map_err(|_| refused())?;
    stream.flush().await.map_err(|_| refused())
}
pub(super) async fn alive(path: &Path, owned_pid: u32) -> Result<bool, SelectionFailure> {
    let before = match std::fs::symlink_metadata(path) {
        Ok(stat) => stat,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err(refused()),
    };
    if !before.file_type().is_socket() || before.uid() != unsafe { nix::libc::geteuid() } {
        return Err(refused());
    }
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        let mut stream = UnixStream::connect(path).await.map_err(|_| refused())?;
        send(&mut stream, &[1, 4]).await?;
        if frame(&mut stream).await? != words(&[1, 4]) {
            return Err(refused());
        }
        send(&mut stream, &[0x10000004, 1]).await?;
        if frame(&mut stream).await? != words(&[0x80000005, 1, owned_pid]) {
            return Err(refused());
        }
        let after = std::fs::symlink_metadata(path).map_err(|_| refused())?;
        if before.dev() != after.dev() || before.ino() != after.ino() {
            return Err(refused());
        }
        Ok(true)
    })
    .await
    .map_err(|_| refused())?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn forwarding_has_no_fresh_network_or_agent_fallback_and_quotes_paths() {
        let upstream = chimaera_link::SshAuthDestination {
            hostname: "bastion.invalid".into(),
            user: "native".into(),
            port: 2222,
        };
        let target = chimaera_link::SshAuthDestination {
            hostname: "2001:db8::1".into(),
            user: "other".into(),
            port: 22,
        };
        let command = forward_proxy(
            Path::new("/private/tmp/owned 'path;$(false)/master"),
            &upstream,
            &target,
        )
        .unwrap();
        assert!(command.contains("-S '/private/tmp/owned '\\''path;$(false)/master'"));
        assert!(command.contains("-W '[2001:db8::1]:22'"));
        for option in [
            "ProxyCommand=/usr/bin/false",
            "ProxyJump=none",
            "PubkeyAuthentication=no",
            "PasswordAuthentication=no",
            "KbdInteractiveAuthentication=no",
            "IdentityAgent=none",
        ] {
            assert!(command.contains(option));
        }
        assert!(forward_proxy(Path::new("/tmp/%h/master"), &upstream, &target).is_err());
    }
    #[tokio::test]
    async fn alive_requires_exact_owned_pid_complete_version_and_response() {
        for mismatch in 0..4 {
            let base = std::fs::canonicalize(std::env::temp_dir()).unwrap();
            let path = base.join(format!(
                "cx-mux-receipt-{}",
                &chimaera_core::generate_token()[..20]
            ));
            let listener = tokio::net::UnixListener::bind(&path).unwrap();
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let _ = frame(&mut stream).await.unwrap();
                if mismatch == 1 {
                    send(&mut stream, &[1, 99]).await.unwrap();
                    return;
                }
                send(&mut stream, &[1, 4]).await.unwrap();
                let _ = frame(&mut stream).await.unwrap();
                if mismatch == 2 {
                    send(&mut stream, &[0x80000005, 1, 456]).await.unwrap();
                } else if mismatch == 3 {
                    stream.write_u32(12).await.unwrap();
                    stream.write_all(&[0; 4]).await.unwrap();
                } else {
                    send(&mut stream, &[0x80000005, 1, 123]).await.unwrap();
                }
            });
            let result = alive(&path, 123).await;
            if mismatch == 0 {
                assert!(result.unwrap());
            } else {
                assert!(result.is_err());
            }
            server.await.unwrap();
            std::fs::remove_file(&path).unwrap();
        }
        assert!(
            !alive(Path::new("/private/tmp/no-such-native-mux-receipt"), 123)
                .await
                .unwrap()
        );
    }
}
