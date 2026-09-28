//! Bounded input preserves command authority until the actual blocking write.
use bytes::Bytes;
use tokio::sync::{mpsc, oneshot};

use crate::ExecError;

pub(crate) type Admission = Box<dyn FnOnce() -> Result<Box<dyn Send>, ExecError> + Send + 'static>;

pub(crate) struct Input {
    bytes: Bytes,
    admission: Option<Admission>,
    completion: Option<oneshot::Sender<Result<(), ExecError>>>,
}

#[derive(Clone)]
pub struct InputSender(mpsc::Sender<Input>);

pub(crate) fn channel(capacity: usize) -> (InputSender, mpsc::Receiver<Input>) {
    let (sender, receiver) = mpsc::channel(capacity);
    (InputSender(sender), receiver)
}

impl InputSender {
    /// Revalidate an admitted request in the writer, retaining its returned
    /// guard until write/flush finishes. Ordinary `send` remains unchanged.
    pub async fn send_authorized<G: Send + 'static>(
        &self,
        bytes: Bytes,
        admission: impl FnOnce() -> Result<G, ExecError> + Send + 'static,
    ) -> Result<(), ExecError> {
        self.send_guarded(
            bytes,
            Box::new(move || admission().map(|guard| Box::new(guard) as Box<dyn Send>)),
        )
        .await
    }
    /// Ordinary terminal input retains its existing bounded queue semantics.
    pub async fn send(&self, bytes: Bytes) -> Result<(), mpsc::error::SendError<Bytes>> {
        self.0
            .send(Input {
                bytes,
                admission: None,
                completion: None,
            })
            .await
            .map_err(|error| mpsc::error::SendError(error.0.bytes))
    }

    pub(crate) async fn send_guarded(
        &self,
        bytes: Bytes,
        admission: Admission,
    ) -> Result<(), ExecError> {
        let (completion, result) = oneshot::channel();
        self.0
            .send(Input {
                bytes,
                admission: Some(admission),
                completion: Some(completion),
            })
            .await
            .map_err(|_| ExecError::SessionGone)?;
        result.await.map_err(|_| ExecError::SessionGone)?
    }
}

impl Input {
    pub(crate) fn write(
        self,
        writer: impl FnOnce(&[u8]) -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        let guard = match self.admission {
            Some(admit) => match admit() {
                Ok(guard) => Some(guard),
                Err(error) => {
                    if let Some(completion) = self.completion {
                        let _ = completion.send(Err(error));
                    }
                    return Ok(());
                }
            },
            None => None,
        };
        // The queue consumer owns this guard. Dropping a disconnected exec
        // future cannot release it while a PTY write is still in progress.
        let result = writer(&self.bytes);
        drop(guard);
        if let Some(completion) = self.completion {
            let _ = completion.send(if result.is_ok() {
                Ok(())
            } else {
                Err(ExecError::SessionGone)
            });
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    };

    #[tokio::test]
    async fn queued_input_rechecks_authority_even_after_its_caller_disconnects() {
        let (sender, mut queue) = channel(1);
        let current = Arc::new(AtomicBool::new(true));
        let authority = current.clone();
        let call = tokio::spawn(async move {
            sender
                .send_guarded(
                    Bytes::from_static(b"stale command\r"),
                    Box::new(move || {
                        if authority.load(Ordering::Acquire) {
                            Ok(Box::new(()))
                        } else {
                            Err(ExecError::Busy("authority changed".into()))
                        }
                    }),
                )
                .await
        });
        let queued = queue.recv().await.unwrap();
        current.store(false, Ordering::Release);
        call.abort();
        let _ = call.await;
        queued
            .write(|_| panic!("stale queued input reached the writer"))
            .unwrap();
    }

    #[tokio::test]
    async fn caller_cancellation_keeps_writer_reservation_until_flush_finishes() {
        struct Reservation(Arc<AtomicUsize>);
        impl Drop for Reservation {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::AcqRel);
            }
        }
        let held = Arc::new(AtomicUsize::new(0));
        let count = held.clone();
        let (sender, mut queue) = channel(1);
        let call = tokio::spawn(async move {
            sender
                .send_guarded(
                    Bytes::from_static(b"command\r"),
                    Box::new(move || {
                        count.fetch_add(1, Ordering::AcqRel);
                        Ok(Box::new(Reservation(count)))
                    }),
                )
                .await
        });
        let queued = queue.recv().await.unwrap();
        let (entered, ready) = tokio::sync::oneshot::channel();
        let (finish, wait) = std::sync::mpsc::channel();
        let writer = tokio::task::spawn_blocking(move || {
            queued.write(|_| {
                entered.send(()).unwrap();
                wait.recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
                Ok(())
            })
        });
        ready.await.unwrap();
        call.abort();
        let _ = call.await;
        assert_eq!(held.load(Ordering::Acquire), 1);
        finish.send(()).unwrap();
        writer.await.unwrap().unwrap();
        assert_eq!(held.load(Ordering::Acquire), 0);
    }
}
