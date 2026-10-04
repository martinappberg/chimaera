//! Explicit nondefault fixture composition, not a normal provider-launch API.
//! The original protected owners remain sealed to the actual host state.
use super::{provider_ready, provider_startup::Pending};
use crate::{lock, AppState};
use chimaera_core::{project_secret_idle::Binding, provider_runtime as wire};
use std::sync::{atomic::Ordering, Arc};

pub use super::provider_client::{ChildLifetime, Observer, Owner};
pub use crate::cloud::providers::process::{group_alive, Child};

/// No state, capability or transport getter. Only host lifecycle and the
/// explicit synthetic test constructor can mint an original context.
#[derive(Clone)]
pub struct Context {
    pub(super) state: Arc<AppState>,
    original: Arc<Pending>,
}
impl Context {
    pub(crate) fn from_state(state: Arc<AppState>) -> Result<Self, wire::Error> {
        let original = lock(&state.pro.execution.provider_pending)
            .clone()
            .ok_or(wire::Error::Inactive)?;
        let context = Self { state, original };
        context.pending()?;
        Ok(context)
    }
    fn pending(&self) -> Result<Arc<Pending>, wire::Error> {
        let pending = lock(&self.state.pro.execution.provider_pending)
            .clone()
            .ok_or(wire::Error::Inactive)?;
        if !Arc::ptr_eq(&pending, &self.original) {
            return Err(wire::Error::StateChanged);
        }
        pending
            .protection
            .current()
            .map_err(|_| wire::Error::StateChanged)?;
        Ok(pending)
    }
    pub fn binding(&self) -> Result<Binding, wire::Error> {
        Ok(self.pending()?.launch.clone())
    }
    pub(super) fn current(&self) -> Result<(), wire::Error> {
        self.pending().map(|_| ())
    }
    pub fn verified(&self) -> Result<bool, wire::Error> {
        self.current()?;
        provider_ready::fixture_verified(&self.state)
    }
    pub fn stopping(&self) -> bool {
        self.state.stopping.load(Ordering::Acquire)
    }
}

/// Synthetic construction is opt-in and uses the original actual Configure /
/// Ready exchange; raw authority mutation remains private to host guard tests.
pub mod testing {
    pub use super::super::provider_ready::test_fixture::Fixture;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    #[tokio::test]
    async fn captured_fixture_context_never_admits_a_replacement_pending() {
        let original = testing::Fixture::new(Duration::from_secs(1));
        original.verified().await;
        let context = original.context();
        let owner = Owner::admit(
            &context,
            wire::Command::GithubGhAccess {},
            Instant::now() + Duration::from_secs(1),
        )
        .unwrap();
        let successor = testing::Fixture::new(Duration::from_secs(1));
        *lock(&original.state.pro.execution.provider_pending) = Some(successor.pending.clone());
        assert!(matches!(context.binding(), Err(wire::Error::StateChanged)));
        assert!(matches!(
            Owner::admit(
                &context,
                wire::Command::GithubGhAccess {},
                Instant::now() + Duration::from_secs(1)
            ),
            Err(wire::Error::StateChanged)
        ));
        *lock(&original.state.pro.execution.provider_pending) = Some(original.pending.clone());
        assert!(context.binding().is_ok());
        drop(owner);
        original.finish().await;
    }
}
