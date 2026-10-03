//! Account mutation admission follows the actual continuation, not its observer.
use std::{future::Future, sync::Arc, time::Duration};
use tokio::sync::{Mutex, OwnedSemaphorePermit};
pub(super) async fn owned<T: Send + 'static>(
    operation: Arc<Mutex<()>>,
    request: OwnedSemaphorePermit,
    writer: Option<OwnedSemaphorePermit>,
    run: impl Future<Output = Result<T, String>> + Send + 'static,
) -> Result<T, String> {
    tokio::spawn(async move {
        let (_request, _writer) = (request, writer);
        let _operation = tokio::time::timeout(Duration::from_secs(5), operation.lock_owned())
            .await
            .map_err(|_| "providers_limit_reached".to_owned())?;
        run.await
    })
    .await
    .map_err(|_| "providers_unconfirmed".to_owned())?
}
#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        sync::Semaphore,
    };
    #[tokio::test]
    async fn caller_loss_keeps_account_and_budgets_until_actual_reply() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let arrived = Arc::new(Semaphore::new(0));
        let release = Arc::new(Semaphore::new(0));
        let server_arrived = arrived.clone();
        let server_release = release.clone();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut input = [0; 1];
            socket.read_exact(&mut input).await.unwrap();
            server_arrived.add_permits(1);
            server_release.acquire().await.unwrap().forget();
            socket.write_all(b"1").await.unwrap();
        });
        let requests = Arc::new(Semaphore::new(1));
        let writers = Arc::new(Semaphore::new(1));
        let account = Arc::new(Mutex::new(()));
        let completed = Arc::new(Semaphore::new(0));
        let completion = completed.clone();
        let observer = tokio::spawn(owned(
            account.clone(),
            requests.clone().try_acquire_owned().unwrap(),
            Some(writers.clone().try_acquire_owned().unwrap()),
            async move {
                let mut socket = TcpStream::connect(address)
                    .await
                    .map_err(|_| "fixed connect".to_owned())?;
                socket
                    .write_all(b"x")
                    .await
                    .map_err(|_| "fixed send".to_owned())?;
                let mut reply = [0; 1];
                socket
                    .read_exact(&mut reply)
                    .await
                    .map_err(|_| "fixed reply".to_owned())?;
                assert_eq!(&reply, b"1");
                completion.add_permits(1);
                Ok(())
            },
        ));
        tokio::time::timeout(Duration::from_secs(5), arrived.acquire())
            .await
            .unwrap()
            .unwrap()
            .forget();
        observer.abort();
        assert!(observer.await.unwrap_err().is_cancelled());
        assert!(account.try_lock().is_err());
        assert_eq!(requests.available_permits(), 0);
        assert_eq!(writers.available_permits(), 0);
        release.add_permits(1);
        tokio::time::timeout(Duration::from_secs(5), completed.acquire())
            .await
            .unwrap()
            .unwrap()
            .forget();
        tokio::time::timeout(Duration::from_secs(5), async {
            while requests.available_permits() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(writers.available_permits(), 1);
        assert!(account.try_lock().is_ok());
        server.await.unwrap();
    }
}
