//! The handles an admission hands shared code: what it captured, what it
//! reserved and what it still has to prove at the final commit. Every type
//! here has an inert form (no token) so shared code never branches on
//! whether an extension is composed.
use std::{any::Any, sync::Arc};

use super::{BoxFuture, Need};
use crate::AppState;

/// The kind of process a launch starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchKind {
    Agent,
    Shell,
}

/// The admission changed between capture and use. The one error type
/// callers match on (`err.is::<Changed>()`).
#[derive(Debug)]
pub struct Changed;
impl std::fmt::Display for Changed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("workspace execution authority changed")
    }
}
impl std::error::Error for Changed {}

/// A counted reservation held until a commit or child registration is
/// done; dropping it releases. Opaque to shared code.
pub struct Reservation {
    _held: Box<dyn Any + Send + Sync>,
}
impl Reservation {
    pub fn new(inner: impl Any + Send + Sync) -> Self {
        Self {
            _held: Box::new(inner),
        }
    }
}

/// A short synchronous hold (it may own a lock guard) kept until a child is
/// registered or a read finished; never held across an await.
pub struct Hold<'a> {
    _held: Option<Box<dyn Held + 'a>>,
}
pub trait Held {}
impl<T> Held for T {}
impl<'a> Hold<'a> {
    pub fn none() -> Self {
        Self { _held: None }
    }
    pub fn new(inner: impl Held + 'a) -> Self {
        Self {
            _held: Some(Box::new(inner)),
        }
    }
}

/// An admission captured before asynchronous work and re-checked at the
/// final commit. `None` inside is the inert admission: it only re-asks
/// [`Need::Execute`].
#[derive(Clone)]
pub struct Admission {
    workspace: String,
    token: Option<Arc<dyn AdmissionToken>>,
}
pub trait AdmissionToken: Send + Sync {
    fn check(&self, state: &AppState) -> anyhow::Result<()>;
    /// The admission generation this was captured at: two admissions of the
    /// same workspace and generation are the same authority.
    fn generation(&self) -> u64;
    fn begin(&self, state: &AppState) -> anyhow::Result<Option<Reservation>>;
    /// The workspace's agents run under the policy's process ownership.
    fn managed(&self, state: &AppState) -> bool;
    fn installer<'a>(
        &'a self,
        state: &'a Arc<AppState>,
        workspace: &'a str,
    ) -> BoxFuture<'a, anyhow::Result<Installer>>;
}
impl Admission {
    pub fn inert(workspace: &str) -> Self {
        Self {
            workspace: workspace.to_owned(),
            token: None,
        }
    }
    pub fn with(workspace: &str, token: Arc<dyn AdmissionToken>) -> Self {
        Self {
            workspace: workspace.to_owned(),
            token: Some(token),
        }
    }
    pub fn check(&self, state: &AppState) -> anyhow::Result<()> {
        match &self.token {
            Some(token) => token.check(state),
            None if state.policy().allows(state, &self.workspace, Need::Execute) => Ok(()),
            None => Err(Changed.into()),
        }
    }
    pub fn managed(&self, state: &AppState) -> bool {
        self.token
            .as_ref()
            .is_some_and(|token| token.managed(state))
    }
    pub fn generation(&self) -> u64 {
        self.token.as_ref().map_or(0, |token| token.generation())
    }
    /// Check, then reserve the final dispatch.
    pub fn begin(&self, state: &AppState) -> anyhow::Result<Option<Reservation>> {
        match &self.token {
            Some(token) => token.begin(state),
            None => self.check(state).map(|()| None),
        }
    }
    /// Admit an installer child under this admission.
    pub async fn installer(
        &self,
        state: &Arc<AppState>,
        workspace: &str,
    ) -> anyhow::Result<Installer> {
        match &self.token {
            Some(token) => token.installer(state, workspace).await,
            None => {
                self.check(state)?;
                Ok(Installer {
                    admission: self.clone(),
                    state: state.clone(),
                    token: None,
                })
            }
        }
    }
}

/// An admitted installer process; its cleanup stays counted until finished.
pub struct Installer {
    admission: Admission,
    state: Arc<AppState>,
    token: Option<Box<dyn InstallerToken>>,
}
pub trait InstallerToken: Send + Sync {
    /// Attach the spawned process group synchronously after spawn.
    fn attach(&mut self, group: u32);
    fn finish(self: Box<Self>) -> BoxFuture<'static, anyhow::Result<()>>;
    /// The installer holds a setup reservation whose process group must be
    /// drained on success.
    fn guarded(&self) -> bool;
}
impl Installer {
    pub fn with(
        admission: Admission,
        state: Arc<AppState>,
        token: Box<dyn InstallerToken>,
    ) -> Self {
        Self {
            admission,
            state,
            token: Some(token),
        }
    }
    pub fn captured(&self) -> Admission {
        self.admission.clone()
    }
    pub fn check(&self) -> anyhow::Result<()> {
        self.admission.check(&self.state)
    }
    pub fn guarded(&self) -> bool {
        self.token.as_ref().is_some_and(|token| token.guarded())
    }
    pub fn attach(&mut self, group: u32) {
        if let Some(token) = &mut self.token {
            token.attach(group);
        }
    }
    pub async fn finish(mut self) -> anyhow::Result<()> {
        match self.token.take() {
            Some(token) => token.finish().await,
            None => Ok(()),
        }
    }
}

/// One admitted launch, held until its child is registered.
pub struct Launch {
    token: Option<Box<dyn LaunchToken>>,
}
pub trait LaunchToken: Send + Sync {
    /// The child runs under the policy's process ownership (a fenceable,
    /// counted process group).
    fn managed(&self) -> bool;
    fn check(&self) -> anyhow::Result<()>;
    fn registered(self: Box<Self>, id: String);
}
impl Launch {
    pub fn inert() -> Self {
        Self { token: None }
    }
    pub fn with(token: Box<dyn LaunchToken>) -> Self {
        Self { token: Some(token) }
    }
    pub fn managed(&self) -> bool {
        self.token.as_ref().is_some_and(|token| token.managed())
    }
    pub fn check(&self) -> anyhow::Result<()> {
        self.token.as_ref().map_or(Ok(()), |token| token.check())
    }
    pub fn registered(self, id: String) {
        if let Some(token) = self.token {
            token.registered(id);
        }
    }
}

/// What a launch adds to an agent's start, beyond its environment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LaunchContext {
    /// The agent continues work an interrupted earlier run left: its pick-up
    /// says to check the files first.
    pub recovery: bool,
    /// With `recovery`: how long before the other machine stopped responding
    /// the saved point it continues from was made (ms), when known. The
    /// pick-up says so, because files written after it stay on that machine.
    pub saved_point_age_ms: Option<u64>,
    /// The project's agents get the daemon's tools even in a terminal agent
    /// that otherwise has none.
    pub tools: bool,
}
