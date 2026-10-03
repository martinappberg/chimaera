//! Admission follows the actual one-shot send after its UI observer leaves.
use chimaera_link::project_secrets::Error;
use std::{sync::Arc, time::Duration};
use tokio::sync::{Mutex, OwnedSemaphorePermit};

pub(super) async fn owned<T: Send + 'static>(
    operation: Arc<Mutex<()>>,
    request: OwnedSemaphorePermit,
    writer: OwnedSemaphorePermit,
    action: impl std::future::Future<Output = Result<T, String>> + Send + 'static,
) -> Result<T, String> {
    tokio::spawn(async move {
        let (_request, _writer) = (request, writer);
        let _operation = tokio::time::timeout(Duration::from_secs(5), operation.lock_owned())
            .await
            .map_err(|_| Error::LimitReached.to_string())?;
        action.await
    })
    .await
    .map_err(|_| Error::Unconfirmed.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::Semaphore;
    #[tokio::test]
    async fn canceled_secret_observer_retains_account_lock_and_actual_http_permits() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        const REQUEST: &[u8] = b"POST /fixed HTTP/1.1\r\nHost: fixture\r\nContent-Length: 14\r\nConnection: close\r\n\r\nsynthetic-only";
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (arrived, arrival) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; REQUEST.len()];
            socket.read_exact(&mut request).await.unwrap();
            assert_eq!(request, REQUEST);
            arrived.send(()).unwrap();
            released.await.unwrap();
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
                .await
                .unwrap();
        });
        let operation = Arc::new(Mutex::new(()));
        let requests = Arc::new(Semaphore::new(1));
        let writers = Arc::new(Semaphore::new(1));
        let (finished, completion) = tokio::sync::oneshot::channel();
        let observer = tokio::spawn(owned(
            operation.clone(),
            requests.clone().try_acquire_owned().unwrap(),
            writers.clone().try_acquire_owned().unwrap(),
            async move {
                tokio::time::timeout(Duration::from_secs(5), async {
                    let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
                    socket.write_all(REQUEST).await.unwrap();
                    let mut response = Vec::new();
                    socket.take(4096).read_to_end(&mut response).await.unwrap();
                    assert!(response.starts_with(b"HTTP/1.1 200 OK\r\n"));
                })
                .await
                .unwrap();
                finished.send(()).unwrap();
                Ok(())
            },
        ));
        tokio::time::timeout(Duration::from_secs(5), arrival)
            .await
            .unwrap()
            .unwrap();
        observer.abort();
        assert!(observer.await.unwrap_err().is_cancelled());
        assert!(operation.try_lock().is_err());
        assert_eq!(requests.available_permits(), 0);
        assert_eq!(writers.available_permits(), 0);
        release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), completion)
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if operation.try_lock().is_ok()
                    && requests.available_permits() == 1
                    && writers.available_permits() == 1
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        server.await.unwrap();
    }
}
