//! Native account/Connect ownership for short authentication grants.
use super::Failure;
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc, Mutex, Weak,
    },
};
use tokio::sync::{watch, OwnedSemaphorePermit, Semaphore};

#[derive(Default)]
struct State {
    generation: u64,
    next: u64,
    attempts: BTreeMap<u64, watch::Sender<bool>>,
    routes: BTreeMap<u64, Weak<RoutePromptContext>>,
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
        for (_, route) in std::mem::take(&mut state.routes) {
            if let Some(route) = route.upgrade() {
                route.stop.send_replace(true);
            }
        }
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
    pub(crate) fn route_prompt(
        &self,
        generation: u64,
        host: &str,
        auth: &chimaera_link::SshRoutePromptAuth,
    ) -> Option<RoutePromptGuard> {
        let state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if state.generation != generation {
            return None;
        }
        for (identity, route) in &state.routes {
            let Some(context) = route.upgrade() else {
                continue;
            };
            if context.host == host && auth.matches(&context.grant, &context.boot) {
                let guard = RoutePromptGuard {
                    registry: Arc::downgrade(&self.0),
                    identity: *identity,
                    context,
                    auth: auth.clone(),
                };
                return guard.active_locked(&state).then_some(guard);
            }
        }
        None
    }
    #[cfg(test)]
    fn has_route(&self, generation: u64, host: &str) -> bool {
        let state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        state.generation == generation
            && state
                .routes
                .values()
                .filter_map(Weak::upgrade)
                .any(|route| {
                    route.host == host
                        && !*route.stop.borrow()
                        && route.deadline > tokio::time::Instant::now()
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
    pub(crate) fn native_prompt(&self, deadline: tokio::time::Instant) -> NativePromptGuard {
        NativePromptGuard {
            registry: self.registry.clone(),
            identity: self.identity,
            cancel: self.cancel.clone(),
            deadline,
        }
    }
    pub(crate) fn bind_route(
        &self,
        host: &str,
        grant: &chimaera_link::SshRouteGrant,
        request: &chimaera_link::SshRouteGrantRequest,
        deadline: tokio::time::Instant,
    ) -> Result<RoutePromptOwner, Failure> {
        grant.validate().map_err(|_| Failure::InvalidRequest)?;
        request.validate().map_err(|_| Failure::InvalidRequest)?;
        if !grant.matches(request)
            || deadline <= tokio::time::Instant::now()
            || *self.cancel.borrow()
        {
            return Err(Failure::Revoked);
        }
        let registry = self.registry.upgrade().ok_or(Failure::Revoked)?;
        let mut state = registry.lock().unwrap_or_else(|e| e.into_inner());
        if !state.attempts.contains_key(&self.identity) || state.routes.contains_key(&self.identity)
        {
            return Err(Failure::Revoked);
        }
        let (stop, _) = watch::channel(false);
        let context = Arc::new(RoutePromptContext {
            host: host.into(),
            grant: grant.clone(),
            boot: request.keeper_boot.clone(),
            deadline,
            signed: AtomicU8::new(0),
            stop,
            cancel: self.cancel.clone(),
        });
        state.routes.insert(self.identity, Arc::downgrade(&context));
        Ok(RoutePromptOwner(context))
    }
}

/// A native trust/unlock probe uses the original explicit Connect admission.
/// There is no keeper grant yet; retaining this handle cannot retain or revive
/// its parent Attempt after account replacement or caller cancellation.
#[derive(Clone)]
pub(crate) struct NativePromptGuard {
    registry: Weak<Mutex<State>>,
    identity: u64,
    cancel: watch::Receiver<bool>,
    deadline: tokio::time::Instant,
}
impl NativePromptGuard {
    fn active_locked(&self, state: &State) -> bool {
        !*self.cancel.borrow()
            && self.deadline > tokio::time::Instant::now()
            && state.attempts.contains_key(&self.identity)
    }
    pub(crate) fn active(&self) -> bool {
        self.registry.upgrade().is_some_and(|registry| {
            let state = registry.lock().unwrap_or_else(|e| e.into_inner());
            self.active_locked(&state)
        })
    }
    pub(crate) async fn stopped(&self) {
        let mut cancel = self.cancel.clone();
        tokio::select! {
            biased;
            _ = cancel.wait_for(|value| *value) => {},
            _ = tokio::time::sleep_until(self.deadline) => {},
        }
    }
    /// Only a bounded synchronous native trust append runs here. Its caller
    /// also retains the account-operation guard; generation/Attempt removal
    /// cannot cross the actual syscall after this final admission.
    pub(crate) fn commit<T>(&self, action: impl FnOnce() -> T) -> Result<T, Failure> {
        let registry = self.registry.upgrade().ok_or(Failure::Revoked)?;
        let state = registry.lock().unwrap_or_else(|e| e.into_inner());
        if !self.active_locked(&state) {
            return Err(Failure::Revoked);
        }
        Ok(action())
    }
}
impl Drop for Attempt {
    fn drop(&mut self) {
        if let Some(registry) = self.registry.upgrade() {
            let mut state = registry.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(route) = state
                .routes
                .remove(&self.identity)
                .and_then(|route| route.upgrade())
            {
                route.stop.send_replace(true);
            }
            if let Some(sender) = state.attempts.remove(&self.identity) {
                sender.send_replace(true);
            }
        }
    }
}

struct RoutePromptContext {
    host: String,
    grant: chimaera_link::SshRouteGrant,
    boot: String,
    deadline: tokio::time::Instant,
    signed: AtomicU8,
    stop: watch::Sender<bool>,
    cancel: watch::Receiver<bool>,
}
pub(crate) struct RoutePromptOwner(Arc<RoutePromptContext>);
impl RoutePromptOwner {
    pub(crate) fn proof(&self) -> RoutePromptProof {
        RoutePromptProof(Arc::downgrade(&self.0))
    }
}
impl Drop for RoutePromptOwner {
    fn drop(&mut self) {
        self.0.stop.send_replace(true);
    }
}
pub(crate) struct RoutePromptProof(Weak<RoutePromptContext>);
impl RoutePromptProof {
    pub(crate) fn signed(&self, leg: u8) {
        if let Some(context) = self.0.upgrade() {
            if context.grant.modes.get(usize::from(leg)) == Some(&chimaera_link::SshRouteMode::Key)
            {
                context.signed.fetch_or(1 << leg, Ordering::SeqCst);
            }
        }
    }
}
impl Drop for RoutePromptProof {
    fn drop(&mut self) {
        if let Some(context) = self.0.upgrade() {
            context.stop.send_replace(true);
        }
    }
}
#[derive(Clone)]
pub(crate) struct RoutePromptGuard {
    registry: Weak<Mutex<State>>,
    identity: u64,
    context: Arc<RoutePromptContext>,
    auth: chimaera_link::SshRoutePromptAuth,
}
impl RoutePromptGuard {
    fn active_locked(&self, state: &State) -> bool {
        state.attempts.contains_key(&self.identity)
            && state
                .routes
                .get(&self.identity)
                .and_then(Weak::upgrade)
                .is_some_and(|route| Arc::ptr_eq(&route, &self.context))
            && !*self.context.stop.borrow()
            && !*self.context.cancel.borrow()
            && self.context.deadline > tokio::time::Instant::now()
            && self.context.grant.policies.as_ref().is_none_or(|policies| {
                policies
                    .get(usize::from(self.auth.leg))
                    .is_some_and(|policy| policy.permits_interaction())
            })
            && (self.auth.mode == chimaera_link::SshRouteMode::Interactive
                || self.context.signed.load(Ordering::SeqCst) & (1 << self.auth.leg) != 0)
    }
    pub(crate) fn active(&self) -> bool {
        self.registry.upgrade().is_some_and(|registry| {
            self.active_locked(&registry.lock().unwrap_or_else(|e| e.into_inner()))
        })
    }
    pub(crate) async fn stopped(&self) {
        let mut stop = self.context.stop.subscribe();
        let mut cancel = self.context.cancel.clone();
        tokio::select! {
            biased;
            _ = stop.wait_for(|value| *value) => {},
            _ = cancel.wait_for(|value| *value) => {},
            _ = tokio::time::sleep_until(self.context.deadline) => {},
        }
    }
    pub(crate) fn prompt(&self, prompt: &str) -> String {
        let destination = &self.auth.destination;
        let mode = if self.auth.mode == chimaera_link::SshRouteMode::Interactive {
            "password/MFA"
        } else {
            "MFA after SSH key verification"
        };
        format!(
            "SSH to {}@{}:{} ({mode})\n{prompt}",
            destination.user, destination.hostname, destination.port
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> chimaera_link::SshRouteGrantRequest {
        use chimaera_link::*;
        let leg = |host: &str, mode| SshRouteAuthLeg {
            policy: None,
            destination: SshAuthDestination {
                hostname: host.into(),
                user: "fixture".into(),
                port: 22,
            },
            mode,
            host_keys: vec![SshAuthHostKey {
                key: "AQ==".into(),
                is_ca: false,
            }],
            user_keys: if mode == SshRouteMode::Key {
                vec!["AQ==".into()]
            } else {
                vec![]
            },
        };
        let first = leg("jump.invalid", SshRouteMode::Key);
        let last = leg("final.invalid", SshRouteMode::Interactive);
        SshRouteGrantRequest {
            version: 1,
            keeper_boot: "boot".into(),
            destination: last.destination.clone(),
            route: SshRoute {
                version: 1,
                jumps: vec![first.destination.clone()],
            },
            legs: vec![first, last],
        }
    }
    fn grant(request: &chimaera_link::SshRouteGrantRequest) -> chimaera_link::SshRouteGrant {
        chimaera_link::SshRouteGrant {
            policies: request.legs.iter().map(|leg| leg.policy.clone()).collect(),
            version: 1,
            grant_id: "grant".into(),
            expires_in: 180,
            destination: request.destination.clone(),
            route: request.route.clone(),
            modes: request.legs.iter().map(|leg| leg.mode).collect(),
        }
    }
    fn auth(
        request: &chimaera_link::SshRouteGrantRequest,
        leg: u8,
    ) -> chimaera_link::SshRoutePromptAuth {
        chimaera_link::SshRoutePromptAuth {
            grant_id: "grant".into(),
            keeper_boot: "boot".into(),
            leg,
            mode: request.legs[usize::from(leg)].mode,
            destination: request.legs[usize::from(leg)].destination.clone(),
        }
    }
    #[tokio::test]
    async fn key_only_policy_never_opens_a_prompt_even_after_its_own_signature() {
        let registry = Registry::default();
        let attempt = registry.admit(0).ok().unwrap();
        let mut request = request();
        for leg in &mut request.legs {
            leg.policy = Some(chimaera_link::SshRoutePolicy {
                version: 1,
                methods: if leg.mode == chimaera_link::SshRouteMode::Key {
                    vec![chimaera_link::SshRouteMethod::Publickey]
                } else {
                    vec![chimaera_link::SshRouteMethod::Password]
                },
                host_key_algorithms: vec!["ssh-ed25519".into()],
                ca_signature_algorithms: vec!["ssh-ed25519".into()],
                pubkey_accepted_algorithms: vec!["ssh-ed25519".into()],
                kex_algorithms: vec!["curve25519-sha256".into()],
                ciphers: vec!["chacha20-poly1305@openssh.com".into()],
                macs: vec!["hmac-sha2-256-etm@openssh.com".into()],
            });
        }
        let receipt = grant(&request);
        let owner = attempt
            .bind_route(
                "host",
                &receipt,
                &request,
                tokio::time::Instant::now() + std::time::Duration::from_secs(1),
            )
            .ok()
            .unwrap();
        let proof = owner.proof();
        proof.signed(0);
        assert!(registry
            .route_prompt(0, "host", &auth(&request, 0))
            .is_none());
        assert!(registry
            .route_prompt(0, "host", &auth(&request, 1))
            .is_some());
    }
    #[tokio::test]
    async fn route_prompt_requires_original_owner_exact_metadata_and_own_leg_signature() {
        let registry = Registry::default();
        let attempt = registry.admit(0).ok().unwrap();
        let request = request();
        let receipt = grant(&request);
        assert!(registry
            .route_prompt(0, "host", &auth(&request, 1))
            .is_none());
        let owner = attempt
            .bind_route(
                "host",
                &receipt,
                &request,
                tokio::time::Instant::now() + std::time::Duration::from_secs(1),
            )
            .ok()
            .unwrap();
        assert!(registry.has_route(0, "host"));
        assert!(registry
            .route_prompt(0, "host", &auth(&request, 0))
            .is_none());
        let proof = owner.proof();
        proof.signed(1); // An interactive leg cannot manufacture a key receipt.
        assert!(registry
            .route_prompt(0, "host", &auth(&request, 0))
            .is_none());
        proof.signed(0);
        assert!(registry
            .route_prompt(0, "host", &auth(&request, 0))
            .unwrap()
            .active());
        let guard = registry
            .route_prompt(0, "host", &auth(&request, 1))
            .unwrap();
        assert!(guard
            .prompt("Challenge?")
            .contains("fixture@final.invalid:22 (password/MFA)"));
        assert!(registry
            .route_prompt(1, "host", &auth(&request, 1))
            .is_none());
        assert!(registry
            .route_prompt(0, "other-host", &auth(&request, 1))
            .is_none());
        for field in 0..6 {
            let mut changed = auth(&request, 1);
            match field {
                0 => changed.grant_id = "other".into(),
                1 => changed.keeper_boot = "other".into(),
                2 => changed.leg = 0,
                3 => changed.mode = chimaera_link::SshRouteMode::Key,
                4 => changed.destination.hostname = "other".into(),
                _ => changed.destination.port = 2222,
            }
            assert!(registry.route_prompt(0, "host", &changed).is_none());
        }
        drop(proof); // Control verifier loss closes all legs synchronously.
        assert!(!guard.active());
        tokio::time::timeout(std::time::Duration::from_millis(100), guard.stopped())
            .await
            .unwrap();
        assert!(!registry.has_route(0, "host"));
        assert!(attempt
            .bind_route(
                "host",
                &receipt,
                &request,
                tokio::time::Instant::now() + std::time::Duration::from_secs(1)
            )
            .is_err());
    }
    #[tokio::test]
    async fn route_prompt_permission_ends_before_detached_cleanup_and_after_expiry_or_account_change(
    ) {
        for exit in 0..3 {
            let registry = Registry::default();
            let attempt = registry.admit(0).ok().unwrap();
            let request = request();
            let owner = attempt
                .bind_route(
                    "host",
                    &grant(&request),
                    &request,
                    tokio::time::Instant::now() + std::time::Duration::from_millis(50),
                )
                .ok()
                .unwrap();
            let guard = registry
                .route_prompt(0, "host", &auth(&request, 1))
                .unwrap();
            match exit {
                0 => drop(owner),
                1 => registry.advance(1),
                _ => tokio::time::sleep(std::time::Duration::from_millis(60)).await,
            }
            assert!(!guard.active());
            tokio::time::timeout(std::time::Duration::from_millis(100), guard.stopped())
                .await
                .unwrap();
            // Observer-held guards retain no authentication budget/owner.
            drop(attempt);
            assert!(registry.admit(if exit == 1 { 1 } else { 0 }).is_ok());
        }
    }
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
    #[tokio::test]
    async fn native_probe_owner_loss_account_change_and_expiry_refuse_real_commit() {
        for exit in 0..3 {
            let registry = Registry::default();
            let attempt = registry.admit(0).ok().unwrap();
            let guard = attempt
                .native_prompt(tokio::time::Instant::now() + std::time::Duration::from_millis(30));
            assert!(guard.active());
            assert_eq!(guard.commit(|| 7).ok(), Some(7));
            match exit {
                0 => drop(attempt),
                1 => registry.advance(1),
                _ => tokio::time::sleep(std::time::Duration::from_millis(40)).await,
            }
            let mut wrote = false;
            assert!(guard.commit(|| wrote = true).is_err());
            assert!(!wrote);
            tokio::time::timeout(std::time::Duration::from_millis(100), guard.stopped())
                .await
                .unwrap();
            assert!(!guard.active());
        }
    }
}
