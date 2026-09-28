//! Browser returns are navigation hints. Only an authenticated account read can
//! confirm a purchase; the bounded task belongs to the shell, not a webview.
use super::super::Shell;
use chimaera_link::{BillingInterval, BillingPortalTarget, Client, DesktopBillingCallback, Plan};
use serde::Serialize;
use std::{
    sync::Mutex,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Emitter, Manager, WebviewWindow};
use tokio::{net::TcpListener, sync::watch, time::Instant};

mod callback;
const WAIT: Duration = Duration::from_secs(15 * 60);
const CONFIRM: Duration = Duration::from_secs(2 * 60);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Kind {
    Checkout,
    Portal,
    PlanChange,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Phase {
    Opening,
    Waiting,
    Confirming,
    Confirmed,
    Canceled,
    Expired,
    Failed,
}
impl Phase {
    fn pending(self) -> bool {
        matches!(self, Self::Opening | Self::Waiting | Self::Confirming)
    }
}
#[derive(Clone, Serialize)]
pub(super) struct Status {
    id: u64,
    kind: Kind,
    requested_plan: Option<Plan>,
    phase: Phase,
    expires_at: u64,
    error: Option<String>,
}
struct Pending {
    status: Status,
    cancel: watch::Sender<bool>,
}
#[derive(Default)]
struct State {
    next: u64,
    pending: Option<Pending>,
}
#[derive(Default)]
pub(super) struct Billing {
    state: Mutex<State>,
}
struct Attempt {
    id: u64,
    generation: u64,
    account_id: String,
    plan: Option<Plan>,
    kind: Kind,
    window: String,
    cancel: watch::Receiver<bool>,
    deadline: Instant,
}
impl Billing {
    pub fn status(&self) -> Option<Status> {
        super::lock(&self.state)
            .pending
            .as_ref()
            .map(|pending| pending.status.clone())
    }
    fn begin(
        &self,
        generation: u64,
        account_id: String,
        plan: Option<Plan>,
        kind: Kind,
        window: String,
    ) -> Attempt {
        let mut state = super::lock(&self.state);
        if let Some(old) = state.pending.take() {
            old.cancel.send_replace(true);
        }
        state.next = state.next.wrapping_add(1);
        let id = state.next;
        let (cancel, receiver) = watch::channel(false);
        state.pending = Some(Pending {
            cancel,
            status: Status {
                id,
                kind,
                requested_plan: plan.clone(),
                phase: Phase::Opening,
                expires_at: now() + WAIT.as_secs(),
                error: None,
            },
        });
        Attempt {
            id,
            generation,
            account_id,
            plan,
            kind,
            window,
            cancel: receiver,
            deadline: Instant::now() + WAIT,
        }
    }
    fn current(&self, id: u64) -> bool {
        super::lock(&self.state)
            .pending
            .as_ref()
            .is_some_and(|pending| pending.status.id == id && !*pending.cancel.borrow())
    }
    fn update(&self, id: u64, phase: Phase, error: Option<&str>) -> bool {
        let mut state = super::lock(&self.state);
        let Some(pending) = state.pending.as_mut().filter(|pending| {
            pending.status.id == id && !*pending.cancel.borrow() && pending.status.phase.pending()
        }) else {
            return false;
        };
        pending.status.phase = phase;
        pending.status.error = error.map(str::to_owned);
        if phase == Phase::Confirming {
            pending.status.expires_at = now() + CONFIRM.as_secs();
        }
        true
    }
    fn cancel(&self, expected: Option<u64>) -> bool {
        let mut state = super::lock(&self.state);
        if expected.is_some_and(|id| {
            state
                .pending
                .as_ref()
                .is_none_or(|pending| pending.status.id != id)
        }) {
            return false;
        }
        if let Some(mut pending) = state.pending.take() {
            pending.cancel.send_replace(true);
            if pending.status.phase.pending() {
                pending.status.phase = Phase::Canceled;
                pending.status.error = None;
                state.pending = Some(pending);
            }
        }
        true
    }
    pub fn clear(&self) {
        if let Some(pending) = super::lock(&self.state).pending.take() {
            pending.cancel.send_replace(true);
        }
    }
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn billing_url(value: &str) -> Result<url::Url, String> {
    let url = url::Url::parse(value).map_err(|_| "The billing link is unavailable. Try again.")?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some_and(|port| port != 443)
        || !matches!(
            url.host_str(),
            Some("checkout.stripe.com" | "billing.stripe.com" | "billing.link.com")
        )
    {
        return Err("The billing link could not be verified. Try again.".into());
    }
    Ok(url)
}

async fn open_billing(
    app: AppHandle,
    window: WebviewWindow,
    checkout: Option<(Plan, BillingInterval)>,
    target: Option<BillingPortalTarget>,
) -> Result<(), String> {
    let state = app.state::<Shell>();
    let (client, generation) = state
        .pro
        .client_snapshot()
        .await
        .ok_or("Sign in before opening billing.")?;
    if checkout
        .as_ref()
        .is_some_and(|(plan, _)| *plan == Plan::None)
    {
        return Err("Choose Pro or Max.".into());
    }
    if let Some(target) = &target {
        target
            .validate()
            .map_err(|_| "Choose Pro or Max.".to_string())?;
    }
    let kind = if checkout.is_some() {
        Kind::Checkout
    } else if target.is_some() {
        Kind::PlanChange
    } else {
        Kind::Portal
    };
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(|_| "Could not prepare the return to chimaera. Try again.".to_string())?;
    let callback = DesktopBillingCallback {
        redirect_uri: format!(
            "http://127.0.0.1:{}/billing/callback",
            listener
                .local_addr()
                .map_err(|_| "Could not prepare billing.")?
                .port()
        ),
        state: chimaera_link::Pkce::new().state,
    };
    let mut attempt = {
        let _operation = state.pro.operation.lock().await;
        if state.pro.generation() != generation {
            return Err("Your account changed. Open billing again.".into());
        }
        let account_id = super::lock(&state.pro.account)
            .as_ref()
            .map(|account| account.account_id.clone())
            .ok_or("Your account is still loading. Try again shortly.")?;
        state.pro.billing.begin(
            generation,
            account_id,
            checkout
                .as_ref()
                .map(|(plan, _)| plan.clone())
                .or_else(|| target.as_ref().map(|target| target.plan.clone())),
            kind,
            window.label().into(),
        )
    };
    let _ = app.emit("pro-changed", ());
    let opened = async {
        let session = if let Some((plan, interval)) = checkout {
            client
                .billing_checkout_with_callback(plan, interval, &callback)
                .await
        } else if let Some(target) = &target {
            client
                .billing_portal_review_with_callback(target, &callback)
                .await
        } else {
            client.billing_portal_with_callback(&callback).await
        }
        .map_err(|_| "Couldn't open billing. Check your connection and try again.".to_string())?;
        let url = billing_url(&session.url)?;
        let _operation = state.pro.operation.lock().await;
        if state.pro.generation() != generation || !state.pro.billing.current(attempt.id) {
            return Err("Your account changed. Open billing again.".into());
        }
        // Spawns only the platform opener; it does not wait for the browser's
        // lifetime. The generation guard and this brief lock serialize launch.
        open::that_detached(url.as_str())
            .map_err(|_| "Couldn't open your browser. Try again.".to_string())
    };
    let result = tokio::select! {
        _ = attempt.cancel.wait_for(|cancel| *cancel) => Err("Billing was canceled.".into()),
        result = tokio::time::timeout_at(attempt.deadline, opened) => result.unwrap_or_else(|_| Err("Billing expired. Try again.".into())),
    };
    if let Err(error) = result {
        state
            .pro
            .billing
            .update(attempt.id, Phase::Failed, Some(&error));
        let _ = app.emit("pro-changed", ());
        return Err(error);
    }
    update(&app, attempt.id, Phase::Waiting, None);
    tokio::spawn(run(app, client, listener, callback, attempt));
    Ok(())
}

fn confirms(
    expected_account: &str,
    requested: Option<&Plan>,
    account: &chimaera_link::Account,
) -> bool {
    account.account_id == expected_account
        && requested.is_none_or(|requested| account.plan == *requested)
}

/// Shares the normal refresh mutex, but does not wait for keeper provisioning.
/// Publishing the account starts/stops its transport; daemon reconciliation
/// remains on the existing bounded background cadence.
async fn refresh(app: &AppHandle, client: &Client, attempt: &Attempt) -> anyhow::Result<bool> {
    let state = app.state::<Shell>();
    let _refresh = state.pro.refresh.lock().await;
    anyhow::ensure!(
        state.pro.generation() == attempt.generation && state.pro.billing.current(attempt.id),
        "billing canceled"
    );
    let account = client.me().await?;
    let _operation = state.pro.operation.lock().await;
    anyhow::ensure!(
        state.pro.generation() == attempt.generation && state.pro.billing.current(attempt.id),
        "billing canceled"
    );
    anyhow::ensure!(account.account_id == attempt.account_id, "account changed");
    let confirmed = confirms(&attempt.account_id, attempt.plan.as_ref(), &account);
    super::publish_account(app, client, account).await;
    let _ = app.emit("pro-changed", ());
    Ok(confirmed)
}

async fn foreground(app: &AppHandle, attempt: &Attempt) {
    let state = app.state::<Shell>();
    let _operation = state.pro.operation.lock().await;
    if state.pro.billing.current(attempt.id) {
        super::return_to_app(app, &attempt.window, attempt.generation);
    }
}
fn update(app: &AppHandle, id: u64, phase: Phase, error: Option<&str>) {
    if app.state::<Shell>().pro.billing.update(id, phase, error) {
        let _ = app.emit("pro-changed", ());
    }
}

async fn run(
    app: AppHandle,
    client: Client,
    listener: TcpListener,
    callback: DesktopBillingCallback,
    mut attempt: Attempt,
) {
    let mut canceled = attempt.cancel.clone();
    let task = async {
        let receive = callback::receive(listener, &callback, attempt.kind == Kind::Checkout);
        tokio::pin!(receive);
        let mut verified = false;
        let mut poll = Instant::now() + Duration::from_secs(15);
        // A confirmed purchase keeps its one-use return listener until the
        // original deadline so a later Stripe redirect still receives a page.
        let accepted = loop {
            tokio::select! {
                result = &mut receive => break result,
                _ = tokio::time::sleep_until(attempt.deadline) => {
                    if !verified { update(&app, attempt.id, Phase::Expired, Some("The browser session timed out. Return to billing to try again; your account will still update when payment is confirmed.")); }
                    return;
                }
                result = async {
                    tokio::time::sleep_until(poll).await;
                    tokio::time::timeout_at(attempt.deadline, refresh(&app, &client, &attempt)).await
                }, if attempt.plan.is_some() && !verified => {
                    if matches!(result, Ok(Ok(true))) {
                        verified = true;
                        update(&app, attempt.id, Phase::Confirmed, None);
                        let _ = tokio::time::timeout_at(attempt.deadline, foreground(&app, &attempt)).await;
                    }
                    poll = Instant::now() + Duration::from_secs(15);
                }
            }
        };
        let Ok(accepted) = accepted else {
            update(&app, attempt.id, Phase::Failed, Some("The browser return was interrupted. Your account will still update automatically."));
            return;
        };
        let outcome = accepted.outcome;
        if outcome == callback::Outcome::Canceled && !verified {
            update(&app, attempt.id, Phase::Canceled, None);
            let _ = tokio::time::timeout(Duration::from_secs(5), foreground(&app, &attempt)).await;
            accepted.finish().await;
            return;
        }
        attempt.deadline = Instant::now() + CONFIRM;
        if !verified {
            update(&app, attempt.id, Phase::Confirming, None);
        }
        let _ = tokio::time::timeout_at(attempt.deadline, async {
            foreground(&app, &attempt).await;
            accepted.finish().await;
        })
        .await;
        if verified {
            return;
        }
        let mut count = 0;
        loop {
            if matches!(
                tokio::time::timeout_at(attempt.deadline, refresh(&app, &client, &attempt)).await,
                Ok(Ok(true))
            ) {
                update(&app, attempt.id, Phase::Confirmed, None);
                return;
            }
            if Instant::now() >= attempt.deadline {
                break;
            }
            count += 1;
            let delay = Duration::from_secs(if count < 5 { 2 } else { 5 });
            tokio::time::sleep_until((Instant::now() + delay).min(attempt.deadline)).await;
        }
        update(&app, attempt.id, Phase::Expired, Some("Confirmation is taking longer than expected. Your account will keep checking in the background; you do not need to pay again."));
    };
    tokio::select! { _ = canceled.wait_for(|cancel| *cancel) => {}, _ = task => {} }
}

#[tauri::command]
pub async fn pro_billing_checkout(
    app: AppHandle,
    window: WebviewWindow,
    plan: Plan,
    interval: BillingInterval,
) -> Result<(), String> {
    open_billing(app, window, Some((plan, interval)), None).await
}
#[tauri::command]
pub async fn pro_billing_portal(
    app: AppHandle,
    window: WebviewWindow,
    target: Option<BillingPortalTarget>,
) -> Result<(), String> {
    open_billing(app, window, None, target).await
}
#[tauri::command]
pub async fn pro_cancel_billing(app: AppHandle, attempt_id: Option<u64>) -> Result<(), String> {
    if !app.state::<Shell>().pro.billing.cancel(attempt_id) {
        return Err("The billing session changed. Check the latest account status.".into());
    }
    let _ = app.emit("pro-changed", ());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn billing_links_accept_only_exact_secure_provider_origins() {
        for host in [
            "checkout.stripe.com",
            "billing.stripe.com",
            "billing.link.com",
        ] {
            assert!(billing_url(&format!("https://{host}/fixture")).is_ok());
        }
        for value in [
            "http://checkout.stripe.com/test",
            "https://checkout.stripe.com.evil.test/test",
            "https://evil.test/checkout.stripe.com",
            "https://name@billing.stripe.com/test",
            "https://billing.stripe.com:8443/test",
            "file:///tmp/billing",
            "javascript:alert(1)",
        ] {
            assert!(billing_url(value).is_err());
        }
    }
    #[test]
    fn confirmation_requires_this_account_and_the_exact_requested_plan() {
        let mut account = chimaera_link::Account {
            account_id: "own".into(),
            email: "fixture@example.test".into(),
            device_id: "device".into(),
            protocol: 1,
            keeper_url: String::new(),
            plan: Plan::None,
            limits: chimaera_link::Limits {
                cloud_hours: 0,
                storage_bytes: 0,
            },
            usage: chimaera_link::Usage {
                cloud_hours: 0.0,
                storage_bytes: 0,
            },
            hours_exhausted: false,
        };
        assert!(!confirms("own", Some(&Plan::Pro), &account));
        account.plan = Plan::Pro;
        assert!(confirms("own", Some(&Plan::Pro), &account));
        assert!(!confirms("own", Some(&Plan::Max), &account));
        assert!(!confirms("other", Some(&Plan::Pro), &account));
        assert!(confirms("own", None, &account));
    }

    #[tokio::test]
    async fn replacement_cancel_and_signout_fence_late_results() {
        let billing = Billing::default();
        let old = billing.begin(
            1,
            "account".into(),
            Some(Plan::Pro),
            Kind::Checkout,
            "home".into(),
        );
        let current = billing.begin(
            1,
            "account".into(),
            Some(Plan::Max),
            Kind::Checkout,
            "home".into(),
        );
        assert!(*old.cancel.borrow());
        assert!(!billing.cancel(Some(old.id)));
        assert!(!*current.cancel.borrow());
        assert!(!billing.update(old.id, Phase::Confirmed, None));
        assert!(billing.update(current.id, Phase::Confirming, None));
        assert!(billing.cancel(None));
        assert!(*current.cancel.borrow());
        assert!(!billing.update(current.id, Phase::Confirmed, None));
        assert_eq!(billing.status().unwrap().phase, Phase::Canceled);
        billing.clear();
        assert!(billing.status().is_none());
    }
    #[tokio::test]
    async fn cancellation_closes_a_listener_even_with_a_partial_browser_request() {
        use tokio::io::AsyncWriteExt;
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let callback = DesktopBillingCallback {
            redirect_uri: format!("http://{address}/billing/callback"),
            state: chimaera_link::Pkce::new().state,
        };
        let billing = Billing::default();
        let mut attempt = billing.begin(
            1,
            "account".into(),
            Some(Plan::Pro),
            Kind::Checkout,
            "home".into(),
        );
        let id = attempt.id;
        let task = tokio::spawn(async move {
            tokio::select! {
                _ = attempt.cancel.wait_for(|value| *value) => {},
                _ = callback::receive(listener, &callback, true) => panic!("incomplete request accepted"),
            }
        });
        let mut browser = tokio::net::TcpStream::connect(address).await.unwrap();
        browser.write_all(b"GET /billing/callback?").await.unwrap();
        assert!(billing.cancel(Some(id)));
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
        assert!(tokio::net::TcpStream::connect(address).await.is_err());
    }

    #[tokio::test]
    async fn a_new_portal_replaces_stopped_status_and_closed_socket_cannot_cancel_it() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let billing = Billing::default();
        let old = billing.begin(1, "account".into(), None, Kind::Portal, "home".into());
        assert!(billing.cancel(Some(old.id)));
        let attempt = billing.begin(1, "account".into(), None, Kind::Portal, "home".into());
        assert_eq!(billing.status().unwrap().phase, Phase::Opening);
        assert!(!billing.cancel(Some(old.id)));
        assert!(billing.update(attempt.id, Phase::Waiting, None));
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let state = chimaera_link::Pkce::new().state;
        let callback = DesktopBillingCallback {
            redirect_uri: format!("http://{address}/billing/callback"),
            state: state.clone(),
        };
        let receive =
            tokio::spawn(
                async move { callback::receive(listener, &callback, false).await.unwrap() },
            );
        // A browser may preconnect then close. It is neither a return nor cancellation.
        drop(tokio::net::TcpStream::connect(address).await.unwrap());
        let mut wrong = tokio::net::TcpStream::connect(address).await.unwrap();
        wrong.write_all(format!("GET /billing/callback?state={state}&outcome=canceled HTTP/1.1\r\nHost: {address}\r\n\r\n").as_bytes()).await.unwrap();
        let mut response = String::new();
        wrong.read_to_string(&mut response).await.unwrap();
        assert!(response.starts_with("HTTP/1.1 400"));
        assert_eq!(billing.status().unwrap().phase, Phase::Waiting);
        assert!(!*attempt.cancel.borrow());
        let mut browser = tokio::net::TcpStream::connect(address).await.unwrap();
        browser.write_all(format!("GET /billing/callback?state={state}&outcome=portal HTTP/1.1\r\nHost: {address}\r\n\r\n").as_bytes()).await.unwrap();
        let accepted = tokio::time::timeout(Duration::from_secs(1), receive)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(accepted.outcome, callback::Outcome::Portal);
        assert!(billing.update(attempt.id, Phase::Confirming, None));
        accepted.finish().await;
        assert_eq!(billing.status().unwrap().phase, Phase::Confirming);
        // The return alone does not confirm entitlement; the account read does.
        assert!(billing.update(attempt.id, Phase::Confirmed, None));
    }

    #[tokio::test]
    async fn observed_purchase_cannot_be_downgraded_by_late_cancel_or_timeout() {
        let billing = Billing::default();
        let attempt = billing.begin(
            1,
            "account".into(),
            Some(Plan::Pro),
            Kind::Checkout,
            "home".into(),
        );
        assert!(billing.update(attempt.id, Phase::Confirmed, None));
        assert!(!billing.update(attempt.id, Phase::Expired, None));
        // Explicitly acknowledging a terminal result clears presentation and
        // releases the remaining listener without changing account entitlement.
        assert!(billing.cancel(None));
        assert!(billing.status().is_none());
        assert!(*attempt.cancel.borrow());
    }
}
