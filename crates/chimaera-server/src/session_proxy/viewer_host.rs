//! Captured route, scope and global resource guards. Selected policy owns its
//! original inline streams/queue; this host never supplies a route selector.
use super::*;
use std::sync::Arc;

pub use super::Viewer as ViewerFrameKind;
pub use axum::extract::ws::{Message as ViewerMessage, WebSocket as ViewerDownstream};
pub use tokio_tungstenite::tungstenite::Message as OwnerMessage;
pub type ViewerStream = super::Upstream;
pub use tokio_tungstenite::tungstenite::Error as OwnerError;
pub fn admission_refusal(answer: Value, text: &str) -> Value {
    crate::ws::command_refusal(answer, text)
}

#[cfg(all(unix, feature = "daemon-extension-fixture"))]
pub mod fixture;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ViewerReach {
    Awake,
    Sleeping,
}
#[derive(Clone, Copy)]
pub enum ViewerIntent {
    Passive,
    Interaction,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ViewerUpgrade {
    Live,
    Kept,
}
#[derive(Clone, Copy)]
pub enum ViewerOwnerKind {
    Cloud,
    Computer,
}
#[derive(Clone, Copy)]
pub enum ViewerEnded {
    Transport,
    Owner(ViewerOwnerKind),
}

/// A scope result belongs to this admission, not a serialized tuple or a
/// successor route. There is no public constructor or Clone implementation.
pub struct ViewerScope {
    identity: Arc<()>,
    reach: Reach,
}
impl ViewerScope {
    pub fn reach(&self) -> ViewerReach {
        match self.reach {
            Reach::Awake => ViewerReach::Awake,
            Reach::Sleeping => ViewerReach::Sleeping,
        }
    }
}

/// One original global reservation. Split transfers ownership without freeing
/// its bytes; a queued/write-held batch must retain it until actual settlement.
pub struct ViewerReservation<'a> {
    budget: &'a HeldBudget,
    bytes: usize,
}
impl ViewerReservation<'_> {
    pub fn try_grow(&mut self, bytes: usize) -> bool {
        if self.bytes.checked_add(bytes).is_none() || !self.budget.reserve(bytes) {
            return false;
        }
        self.bytes += bytes;
        true
    }
    pub fn shrink(&mut self, bytes: usize) {
        assert!(bytes <= self.bytes);
        self.bytes -= bytes;
        self.budget.release(bytes);
    }
    pub fn split(&mut self, bytes: usize) -> Option<Self> {
        if bytes > self.bytes {
            return None;
        }
        self.bytes -= bytes;
        Some(Self {
            budget: self.budget,
            bytes,
        })
    }
}
impl Drop for ViewerReservation<'_> {
    fn drop(&mut self) {
        self.budget.release(self.bytes);
    }
}

/// Non-Clone original local admission. The same socket permit remains owned
/// through private pending IO/stream cleanup; no token/address is exposed.
pub struct ViewerAdmission<'a> {
    link: Link<'a>,
    identity: Arc<()>,
    interaction: bool,
    _permit: tokio::sync::SemaphorePermit<'static>,
}
impl<'a> ViewerAdmission<'a> {
    pub(super) fn new(
        link: Link<'a>,
        interaction: bool,
        permit: tokio::sync::SemaphorePermit<'static>,
    ) -> Self {
        Self {
            link,
            identity: Arc::new(()),
            interaction,
            _permit: permit,
        }
    }
    pub fn chat(&self) -> bool {
        self.link.chat
    }
    pub fn cloud(&self) -> bool {
        self.link.cloud()
    }
    pub fn interaction(&self) -> bool {
        self.interaction
    }
    pub fn current(&self) -> std::result::Result<(), ViewerEnded> {
        match self
            .link
            .state
            .session_proxy
            .change(&self.link.route, &self.link.workspace)
        {
            RouteChange::Current => Ok(()),
            RouteChange::Transport => Err(ViewerEnded::Transport),
            RouteChange::Owner => {
                let to = self.link.moved();
                Err(ViewerEnded::Owner(if to["to"] == "cloud" {
                    ViewerOwnerKind::Cloud
                } else {
                    ViewerOwnerKind::Computer
                }))
            }
        }
    }
    pub async fn scope(&self) -> std::result::Result<ViewerScope, ()> {
        self.current().map_err(|_| ())?;
        let reach = verify_scope(
            &self.link.state.session_proxy,
            &self.link.route,
            &self.link.workspace,
        )
        .await
        .map_err(|_| ())?;
        self.current().map_err(|_| ())?;
        Ok(ViewerScope {
            identity: self.identity.clone(),
            reach,
        })
    }
    pub async fn connect(
        &self,
        scope: ViewerScope,
        intent: ViewerIntent,
    ) -> std::result::Result<(ViewerStream, ViewerUpgrade), ()> {
        self.current().map_err(|_| ())?;
        if !Arc::ptr_eq(&scope.identity, &self.identity) {
            return Err(());
        }
        let interaction = matches!(intent, ViewerIntent::Interaction) && !self.link.read_only;
        let reach = if interaction {
            scope.reach
        } else {
            Reach::Awake
        };
        let opened = self
            .link
            .connect(interaction, reach)
            .await
            .map_err(|_| ())?;
        self.current().map_err(|_| ())?;
        match opened {
            Opened::Live(stream) => Ok((*stream, ViewerUpgrade::Live)),
            Opened::Held(stream) => Ok((*stream, ViewerUpgrade::Kept)),
        }
    }
    pub fn passive_attach_allowed(&self) -> bool {
        self.cloud()
            && !self
                .link
                .state
                .session_proxy
                .passive_refused(&self.link.route)
    }
    pub fn note_passive_refused(&self) {
        self.link
            .state
            .session_proxy
            .note_passive_refused(&self.link.route);
    }
    pub fn downstream_text(&self, text: &str) -> (String, bool) {
        self.link.downstream_text(text)
    }
    pub fn owner_kind(&self) -> ViewerOwnerKind {
        if self.cloud() {
            ViewerOwnerKind::Cloud
        } else {
            ViewerOwnerKind::Computer
        }
    }
    pub fn reserve(&self, bytes: usize) -> Option<ViewerReservation<'static>> {
        HELD_BUDGET.reserve(bytes).then_some(ViewerReservation {
            budget: &HELD_BUDGET,
            bytes,
        })
    }
    pub async fn optional_unavailable(self, downstream: &mut ViewerDownstream) {
        let _ = bounded_send(downstream, Down::Text(unavailable().to_string().into())).await;
    }
}

/// Ready ordinary remote forwarding remains shared. No sleeping-target attach,
/// wake policy or alternative route is available without the selected runtime.
pub(super) async fn ready(admission: ViewerAdmission<'_>, downstream: &mut ViewerDownstream) {
    let result: Result<()> = async {
        let scope = admission.scope().await.map_err(|_| anyhow::anyhow!("viewer unavailable"))?;
        if scope.reach() == ViewerReach::Sleeping { bail!("viewer unavailable"); }
        let (mut upstream, upgrade) = admission.connect(scope, ViewerIntent::Passive).await.map_err(|_| anyhow::anyhow!("viewer unavailable"))?;
        if upgrade == ViewerUpgrade::Kept { bail!("viewer policy unavailable"); }
        let mut ready = false;
        let mut ownership = tokio::time::interval(Duration::from_secs(2));
        ownership.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = ownership.tick() => { admission.current().map_err(|_| anyhow::anyhow!("viewer retired"))?; }
                next = upstream.next() => match next {
                    Some(Ok(Up::Text(text))) => {
                        admission.current().map_err(|_| anyhow::anyhow!("viewer retired"))?;
                        let (text, actual_ready) = admission.downstream_text(&text);
                        bounded_send(downstream, Down::Text(text.into())).await?;
                        ready |= actual_ready;
                        admission.current().map_err(|_| anyhow::anyhow!("viewer retired"))?;
                    }
                    Some(Ok(Up::Binary(bytes))) => { bounded_send(downstream, Down::Binary(bytes)).await?; }
                    Some(Ok(Up::Ping(bytes))) => { let _ = bounded_send(&mut upstream, Up::Pong(bytes)).await; }
                    Some(Ok(Up::Close(_))) | None | Some(Err(_)) => return Ok(()),
                    _ => {},
                },
                next = downstream.recv() => match next {
                    Some(Ok(frame @ (Down::Text(_) | Down::Binary(_)))) => {
                        admission.current().map_err(|_| anyhow::anyhow!("viewer retired"))?;
                        if ready {
                            if let Some(frame) = upward(frame) { bounded_send(&mut upstream, frame).await?; }
                        } else if matches!(ViewerFrameKind::of(admission.chat(), &frame), ViewerFrameKind::Input { .. } | ViewerFrameKind::Setting { .. }) {
                            // Missing policy never creates a deferred mutation.
                            // Still consume close/input before ready so a silent
                            // peer cannot keep a disconnected viewer's permit.
                            let answer = match &frame {
                                Down::Text(text) => admission_refusal(json!({"type":"error","code":"command_failed","message":"Not sent. Your project is reconnecting."}), text),
                                _ => json!({"type":"error","code":"read_only","reason":"reconnecting","message":"Your project is reconnecting. That input was not sent."}),
                            };
                            bounded_send(downstream, Down::Text(answer.to_string().into())).await?;
                        }
                        admission.current().map_err(|_| anyhow::anyhow!("viewer retired"))?;
                    }
                    Some(Ok(Down::Close(_))) | None | Some(Err(_)) => return Ok(()),
                    _ => {},
                }
            }
        }
    }.await;
    if result.is_err() {
        if let Err(ViewerEnded::Owner(to)) = admission.current() {
            let to = match to {
                ViewerOwnerKind::Cloud => "cloud",
                ViewerOwnerKind::Computer => "computer",
            };
            let _ = bounded_send(
                downstream,
                Down::Text(json!({"type":"moved","to":to}).to_string().into()),
            )
            .await;
        }
    }
}

#[cfg(all(unix, any(test, feature = "daemon-extension-fixture")))]
pub mod legacy_fixture;
