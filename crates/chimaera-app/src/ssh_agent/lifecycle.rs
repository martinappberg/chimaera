//! Native account/Connect ownership for short authentication grants.
use super::Failure;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, Weak},
};
use tokio::sync::{watch, OwnedSemaphorePermit, Semaphore};

#[derive(Default)]
struct State {
    generation: u64,
    next: u64,
    attempts: BTreeMap<u64, watch::Sender<bool>>,
}
#[derive(Clone)]
pub(crate) struct Registry(Arc<Mutex<State>>, Arc<Semaphore>);
impl Default for Registry {
    fn default() -> Self {
        Self(Arc::default(), Arc::new(Semaphore::new(4)))
    }
}
impl Registry {
    pub(crate) fn advance(&self, generation: u64) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        state.generation = generation;
        for (_, cancel) in std::mem::take(&mut state.attempts) {
            cancel.send_replace(true);
        }
    }
    pub(crate) fn admit(&self, generation: u64) -> Result<Attempt, Failure> {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if generation != state.generation {
            return Err(Failure::Revoked);
        }
        if state.attempts.len() >= 4 {
            return Err(Failure::Unavailable);
        }
        // Cleanup retains this permit even across account replacement. Rapid
        // sign-in/cancel cycles cannot create unbounded HTTP cleanup tasks.
        let permit = self
            .1
            .clone()
            .try_acquire_owned()
            .map_err(|_| Failure::Unavailable)?;
        let next = state.next.checked_add(1).ok_or(Failure::Unavailable)?;
        let (sender, cancel) = watch::channel(false);
        state.next = next;
        state.attempts.insert(next, sender);
        Ok(Attempt {
            registry: Arc::downgrade(&self.0),
            identity: next,
            cancel,
            _permit: permit,
        })
    }
}
pub(crate) struct Attempt {
    registry: Weak<Mutex<State>>,
    identity: u64,
    cancel: watch::Receiver<bool>,
    _permit: OwnedSemaphorePermit,
}
impl Attempt {
    pub(crate) fn cancellation(&self) -> watch::Receiver<bool> {
        self.cancel.clone()
    }
}
impl Drop for Attempt {
    fn drop(&mut self) {
        if let Some(registry) = self.registry.upgrade() {
            let mut state = registry.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(sender) = state.attempts.remove(&self.identity) {
                sender.send_replace(true);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn account_change_and_owner_loss_cancel_bounded_attempts_without_touching_replacements() {
        let registry = Registry::default();
        let first = registry.admit(0).ok().unwrap();
        let mut cancelled = first.cancellation();
        let held: Vec<_> = (0..3).map(|_| registry.admit(0).ok().unwrap()).collect();
        assert!(matches!(registry.admit(0), Err(Failure::Unavailable)));
        registry.advance(1);
        cancelled.wait_for(|value| *value).await.unwrap();
        assert!(matches!(registry.admit(0), Err(Failure::Revoked)));
        assert!(matches!(registry.admit(1), Err(Failure::Unavailable)));
        drop(held);
        let replacement = registry.admit(1).ok().unwrap();
        let replacement_cancel = replacement.cancellation();
        drop(first);
        assert!(!*replacement_cancel.borrow());
        drop(replacement);
        assert!(*replacement_cancel.borrow());
        let last = registry.admit(1).ok().unwrap();
        let mut cancel = last.cancellation();
        drop(registry);
        assert!(cancel.changed().await.is_err());
    }
}
