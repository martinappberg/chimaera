//! The public plan catalog: display prices fetched from the account service's
//! credential-free `GET /v1/plans`, so the Pro page can show them before
//! anyone has signed in. Presentation only and best effort: it never gates
//! the status, sets no error or warning, and a failure keeps whatever was
//! last read. Prices are never built into the app; a signed-in `/v1/me`
//! answer that carries its own `plans` takes precedence (see
//! `Pro::status_snapshot`).
use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

use anyhow::Result;
use chimaera_link::{Client, PlanPrice};

/// An answered catalog (including "no prices") is reused this long.
const FRESH: Duration = Duration::from_secs(300);
/// After a failed fetch, the next try waits at least this long.
const RETRY: Duration = Duration::from_secs(60);

#[derive(Default)]
pub(super) struct Catalog {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// The last answered list; kept (even when stale) through later failures.
    prices: Option<Vec<PlanPrice>>,
    /// When the last attempt ended and whether the service answered.
    settled: Option<(Instant, bool)>,
    fetching: bool,
}

impl State {
    fn due(&self, now: Instant) -> bool {
        !self.fetching
            && self.settled.is_none_or(|(at, answered)| {
                now.saturating_duration_since(at) >= if answered { FRESH } else { RETRY }
            })
    }
}

/// The right to fetch now. Dropping it unfinished (the task was cancelled)
/// releases the claim without recording an answer.
pub(super) struct Attempt<'a> {
    catalog: &'a Catalog,
    done: bool,
}

impl Catalog {
    pub(super) fn prices(&self) -> Option<Vec<PlanPrice>> {
        super::lock(&self.state).prices.clone()
    }

    /// Whether a fetch would start now: nothing in flight and nothing fresh.
    pub(super) fn due(&self, now: Instant) -> bool {
        super::lock(&self.state).due(now)
    }

    /// Claims the fetch; `None` while one is running or the answer is fresh.
    pub(super) fn begin(&self, now: Instant) -> Option<Attempt<'_>> {
        let mut state = super::lock(&self.state);
        if !state.due(now) {
            return None;
        }
        state.fetching = true;
        Some(Attempt {
            catalog: self,
            done: false,
        })
    }
}

impl Attempt<'_> {
    /// Records the outcome; true when the prices the page would show changed.
    /// An answer replaces the list (an empty or absent one means no prices);
    /// a failure keeps the last list and only delays the next try.
    pub(super) fn finish(mut self, now: Instant, answer: Result<Option<Vec<PlanPrice>>>) -> bool {
        self.done = true;
        let mut state = super::lock(&self.catalog.state);
        state.fetching = false;
        match answer {
            Ok(prices) => {
                let prices = prices.filter(|prices| !prices.is_empty());
                state.settled = Some((now, true));
                std::mem::replace(&mut state.prices, prices) != state.prices
            }
            Err(_) => {
                state.settled = Some((now, false));
                false
            }
        }
    }
}

impl Drop for Attempt<'_> {
    fn drop(&mut self) {
        if !self.done {
            super::lock(&self.catalog.state).fetching = false;
        }
    }
}

/// One best-effort fetch, honoring the cache and the one-at-a-time claim.
/// True when the catalog's prices changed. The unauthenticated client is
/// built per fetch (a few times an hour at most) and holds nothing.
pub(super) async fn refresh(endpoint: &str, catalog: &Catalog) -> bool {
    let Some(attempt) = catalog.begin(Instant::now()) else {
        return false;
    };
    let answer = async { Client::new(endpoint, None)?.plans().await }.await;
    attempt.finish(Instant::now(), answer)
}

#[cfg(test)]
/// A loopback service that answers each request once with `answers[i]`
/// (status line, body) and reports the request lines it saw.
pub(super) async fn stub_service(
    answers: Vec<(&'static str, &'static str)>,
) -> (String, tokio::task::JoinHandle<Vec<String>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let mut seen = Vec::new();
        for (status, body) in answers {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = [0; 8192];
            let n = stream.read(&mut bytes).await.unwrap();
            let head = String::from_utf8_lossy(&bytes[..n]).into_owned();
            assert!(
                !head.to_ascii_lowercase().contains("authorization"),
                "the catalog is credential-free: {head}"
            );
            seen.push(head.lines().next().unwrap_or_default().to_owned());
            let reply = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            stream.write_all(reply.as_bytes()).await.unwrap();
            stream.shutdown().await.unwrap();
        }
        seen
    });
    (endpoint, server)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chimaera_link::{BillingInterval, Plan};
    fn offer(plan: Plan, interval: BillingInterval, amount_cents: u64) -> PlanPrice {
        PlanPrice {
            plan,
            interval,
            amount_cents,
            currency: "usd".into(),
            cloud_time_multiple: None,
            storage_multiple: None,
        }
    }

    #[test]
    fn an_answer_is_reused_for_five_minutes_and_a_failure_retries_after_one() {
        let catalog = Catalog::default();
        let start = Instant::now();
        assert!(catalog.due(start));
        let attempt = catalog.begin(start).expect("nothing is held yet");
        assert!(catalog.begin(start).is_none(), "one fetch at a time");
        assert!(!catalog.due(start));
        let priced = vec![offer(Plan::Pro, BillingInterval::Month, 1)];
        assert!(attempt.finish(start, Ok(Some(priced.clone()))));
        assert_eq!(catalog.prices(), Some(priced.clone()));
        assert!(!catalog.due(start + FRESH - Duration::from_secs(1)));
        // A stale list keeps showing until a newer answer replaces it.
        let later = start + FRESH;
        assert!(catalog.due(later));
        assert_eq!(catalog.prices(), Some(priced.clone()));
        // A failure changes nothing shown and only delays the next try.
        let attempt = catalog.begin(later).unwrap();
        assert!(!attempt.finish(later, Err(anyhow::anyhow!("offline"))));
        assert_eq!(catalog.prices(), Some(priced.clone()));
        assert!(!catalog.due(later + RETRY - Duration::from_secs(1)));
        assert!(catalog.due(later + RETRY));
        // The same answer again is not a change; an empty one withdraws prices.
        let again = later + RETRY;
        assert!(!catalog
            .begin(again)
            .unwrap()
            .finish(again, Ok(Some(priced))));
        let next = again + FRESH;
        assert!(catalog.begin(next).unwrap().finish(next, Ok(Some(vec![]))));
        assert_eq!(catalog.prices(), None);
    }

    #[test]
    fn a_cancelled_fetch_releases_the_claim_without_recording_an_answer() {
        let catalog = Catalog::default();
        let now = Instant::now();
        drop(catalog.begin(now).unwrap());
        assert!(
            catalog.due(now),
            "an abandoned fetch does not look answered"
        );
        assert_eq!(catalog.prices(), None);
    }

    #[tokio::test]
    async fn the_service_is_asked_once_and_its_answer_is_reused() {
        let (endpoint, server) = stub_service(vec![(
            "200 OK",
            r#"{"plans":[{"plan":"pro","interval":"month","amount_cents":111,"currency":"usd"},
                {"plan":"max","interval":"month","amount_cents":222,"currency":"USD"},
                {"plan":"team","interval":"month","amount_cents":1,"currency":"usd"}]}"#,
        )])
        .await;
        let catalog = Catalog::default();
        assert!(refresh(&endpoint, &catalog).await);
        assert_eq!(
            catalog.prices(),
            Some(vec![
                offer(Plan::Pro, BillingInterval::Month, 111),
                offer(Plan::Max, BillingInterval::Month, 222),
            ])
        );
        // Fresh: no second request (the stub accepts exactly one).
        assert!(!refresh(&endpoint, &catalog).await);
        assert_eq!(server.await.unwrap(), ["GET /v1/plans HTTP/1.1"]);
    }

    #[tokio::test]
    async fn an_older_account_without_the_route_means_no_prices() {
        let (endpoint, server) =
            stub_service(vec![("404 Not Found", r#"{"error":"not_found"}"#)]).await;
        let catalog = Catalog::default();
        assert!(
            !refresh(&endpoint, &catalog).await,
            "no prices is no change"
        );
        assert_eq!(catalog.prices(), None);
        assert!(
            !catalog.due(Instant::now()),
            "the answer is cached like any"
        );
        server.await.unwrap();
    }
}
