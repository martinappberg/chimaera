//! One caller-owned grant socket. No reconnect, replay or background signing.
use super::{Failure, GrantVerifier, LocalAgent, SshAuthReply, SshAuthRequest};
use chimaera_link::SSH_AUTH_FRAME_MAX;
use futures_util::{Sink, SinkExt, Stream, StreamExt};
use tokio::{
    sync::{mpsc, watch},
    time::{timeout_at, Duration, Instant},
};
use tokio_tungstenite::tungstenite::Message;

enum Pending {
    Request(SshAuthRequest),
    Control(Message),
}

/// `socket` has already passed the exact grant Ready check. The verifier holds
/// the original native selection, and its deadline starts before grant creation.
/// The account/Connect owner must revoke or drop `cancel` on sign-out, replacement
/// or cancellation. Dropping this future closes every local agent connection.
pub(crate) async fn run<A, W, E>(
    mut verifier: GrantVerifier<A>,
    socket: W,
    mut cancel: watch::Receiver<bool>,
) -> Result<(), Failure>
where
    A: LocalAgent,
    W: Stream<Item = Result<Message, E>> + Sink<Message, Error = E> + Unpin,
{
    let deadline = verifier.deadline;
    let (mut writer, mut reader) = socket.split();
    let (tx, mut rx) = mpsc::channel::<Pending>(16);
    let incoming = async {
        let mut last_request = 0;
        loop {
            let message = match reader.next().await {
                Some(Ok(Message::Text(text))) if text.len() <= SSH_AUTH_FRAME_MAX => {
                    let Ok(request) = SshAuthRequest::from_frame(text.as_bytes()) else {
                        return Failure::InvalidRequest;
                    };
                    if let SshAuthRequest::SessionBind { request_id, .. }
                    | SshAuthRequest::Sign { request_id, .. } = &request
                    {
                        if *request_id <= last_request {
                            return Failure::InvalidRequest;
                        }
                        last_request = *request_id;
                    }
                    Pending::Request(request)
                }
                Some(Ok(message @ (Message::Ping(_) | Message::Pong(_))))
                    if message.len() <= 125 =>
                {
                    Pending::Control(message)
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return Failure::Unavailable,
                _ => return Failure::InvalidRequest,
            };
            // An overflowing peer loses the entire grant; waiting on a full
            // queue would hide its closure while a local agent waits for touch.
            if tx.try_send(message).is_err() {
                return Failure::InvalidRequest;
            }
        }
    };
    let outgoing = async {
        while let Some(message) = rx.recv().await {
            let (reply, failed) = match message {
                Pending::Request(request) => {
                    let Some(reply) = verifier.handle(request).await? else {
                        continue;
                    };
                    let failed = match &reply {
                        SshAuthReply::Failure { error, .. } => Some(*error),
                        _ => None,
                    };
                    let bytes =
                        serde_json::to_string(&reply).map_err(|_| Failure::InvalidRequest)?;
                    if bytes.len() > SSH_AUTH_FRAME_MAX {
                        return Err(Failure::InvalidRequest);
                    }
                    (Message::Text(bytes.into()), failed)
                }
                Pending::Control(Message::Ping(bytes)) => (Message::Pong(bytes), None),
                Pending::Control(Message::Pong(_)) => continue,
                _ => return Err(Failure::InvalidRequest),
            };
            timeout_at(
                deadline.min(Instant::now() + Duration::from_secs(30)),
                writer.send(reply),
            )
            .await
            .map_err(|_| Failure::Expired)?
            .map_err(|_| Failure::Unavailable)?;
            if let Some(error) = failed {
                return Err(error);
            }
        }
        Err(Failure::Unavailable)
    };
    tokio::select! {
        biased;
        _ = cancel.wait_for(|cancelled| *cancelled) => Err(Failure::Revoked),
        _ = tokio::time::sleep_until(deadline) => Err(Failure::Expired),
        error = incoming => Err(error),
        result = outgoing => result,
    }
}
