//! Serializable v0 account and keeper messages. See PROTOCOL.md for lifecycle rules.
use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 0;
pub const MAX_DATA_FRAME: usize = 64 * 1024;
pub const MAX_CONTROL_FRAME: usize = 128 * 1024;
pub const MAX_IN_FLIGHT: usize = 16;
pub const MAX_STREAMS: usize = 128;

/// Service enums gain values additively (PROTOCOL.md). A value this client
/// does not know decodes as `Unknown` instead of failing the whole response;
/// callers treat it as "not something I can act on".
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Plan {
    None,
    Pro,
    Max,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkerState {
    NoPlan,
    Unavailable,
    Preparing,
    Ready,
    Sleeping,
    Limited,
    Error,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkerReason {
    ProvisioningDisabled,
    BetaInviteRequired,
    HoursExhausted,
    StorageExhausted,
    SpendLimitReached,
    ProvisioningFailed,
    #[serde(other)]
    Unknown,
}

/// Account-confirmed preparation stage, not an estimate or daemon readiness.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkerPhase {
    Keeper,
    Worker,
    Connecting,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkerStatus {
    pub state: WorkerState,
    pub reason: Option<WorkerReason>,
    /// Present only while preparing; older services omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<WorkerPhase>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BillingInterval {
    Month,
    Year,
}

/// A hosted subscription-change review, never an instruction to mutate billing.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BillingPortalTarget {
    pub plan: Plan,
    pub interval: BillingInterval,
}
impl BillingPortalTarget {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            matches!(self.plan, Plan::Pro | Plan::Max),
            "choose Pro or Max"
        );
        Ok(())
    }
}

/// A single native billing attempt. The nonce is not an account credential, but
/// must not be logged or exposed to another window/attempt.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopBillingCallback {
    pub redirect_uri: String,
    pub state: String,
}
impl DesktopBillingCallback {
    pub fn validate(&self) -> anyhow::Result<()> {
        use base64::Engine;
        let url = url::Url::parse(&self.redirect_uri)?;
        let port = url.port().filter(|port| *port >= 1024);
        if port.is_none()
            || self.redirect_uri
                != format!("http://127.0.0.1:{}/billing/callback", port.unwrap_or(0))
            || self.state.len() != 43
            || base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(&self.state)
                .map_or(true, |bytes| bytes.len() != 32)
        {
            anyhow::bail!("invalid desktop billing callback");
        }
        Ok(())
    }
}

// Hosted billing URLs are temporary capabilities; never include them in Debug.
#[derive(Clone, Serialize, Deserialize)]
pub struct BillingSession {
    pub url: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Limits {
    pub cloud_hours: u64,
    pub storage_bytes: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Usage {
    pub cloud_hours: f64,
    pub storage_bytes: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Account {
    pub account_id: String,
    pub email: String,
    pub plan: Plan,
    pub device_id: String,
    pub protocol: u32,
    pub keeper_url: String,
    pub limits: Limits,
    pub usage: Usage,
    pub hours_exhausted: bool,
    /// Additive: the subscription needs a payment method update. Older
    /// services omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payment_due: Option<bool>,
    /// Additive: the billing provider's subscription status, when exposed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription_status: Option<String>,
    /// Additive: the account's current offers. Clients never hardcode prices;
    /// without this list they show plan names only.
    #[serde(
        default,
        deserialize_with = "plan_prices",
        skip_serializing_if = "Option::is_none"
    )]
    pub plans: Option<Vec<PlanPrice>>,
    /// Additive: once a plan has ended, the RFC 3339 time until which its cloud
    /// work can still be brought home; null or absent otherwise. Presentation
    /// only: a value that is not a timestamp is dropped, never failing the read.
    #[serde(
        default,
        deserialize_with = "returning_until",
        skip_serializing_if = "Option::is_none"
    )]
    pub returning_until: Option<String>,
}
impl Account {
    /// A lapsed payment reads as plan `none` on older services; this is the
    /// explicit signal when the service provides one.
    pub fn needs_payment(&self) -> bool {
        self.payment_due.unwrap_or(false)
            || matches!(
                self.subscription_status.as_deref(),
                Some("past_due" | "unpaid")
            )
    }
}

/// One offered plan price. Amounts are minor units of `currency` (ISO 4217,
/// lowercase), exactly as the account supplies them.
///
/// `cloud_time_multiple` and `storage_multiple` say how many times the plan's
/// monthly cloud time and storage are the Pro plan's, as whole numbers (Pro's
/// own entries carry 1). The list carries no absolute allowance; a subscribed
/// account's own `Limits` stay on the account. Additive: an older service omits
/// them, and a value that is not a positive integer reads as absent without
/// dropping the row (a multiple is presentation, the price still shows).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanPrice {
    pub plan: Plan,
    pub interval: BillingInterval,
    pub amount_cents: u64,
    pub currency: String,
    #[serde(
        default,
        deserialize_with = "multiple",
        skip_serializing_if = "Option::is_none"
    )]
    pub cloud_time_multiple: Option<u32>,
    #[serde(
        default,
        deserialize_with = "multiple",
        skip_serializing_if = "Option::is_none"
    )]
    pub storage_multiple: Option<u32>,
}
impl PlanPrice {
    fn valid(&self) -> bool {
        matches!(self.plan, Plan::Pro | Plan::Max)
            && self.amount_cents <= 100_000_000
            && self.currency.len() == 3
            && self.currency.bytes().all(|byte| byte.is_ascii_lowercase())
    }
}
/// A multiple is a positive whole number that fits `u32`; anything else (zero,
/// a string, a fraction, a negative, null, an oversized number) is "not
/// stated", never an error, so one odd number cannot take a price off the page.
fn multiple<'de, D>(deserializer: D) -> Result<Option<u32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(value
        .as_ref()
        .and_then(serde_json::Value::as_u64)
        .and_then(|times| u32::try_from(times).ok())
        .filter(|times| *times > 0))
}
/// The public catalog, `GET /v1/plans`: the same offers list as
/// `Account::plans`, read identically, without an account. `plans` is null
/// before the service has a catalog (or when the body has none).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanCatalog {
    #[serde(default, deserialize_with = "plan_prices")]
    pub plans: Option<Vec<PlanPrice>>,
}
/// Pricing is presentation only: an entry this client cannot interpret (a new
/// plan, interval or malformed amount) is dropped instead of failing the
/// whole account read.
fn plan_prices<'de, D>(deserializer: D) -> Result<Option<Vec<PlanPrice>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(match value {
        Some(serde_json::Value::Array(rows)) => Some(
            rows.into_iter()
                .take(16)
                .filter_map(|row| serde_json::from_value::<PlanPrice>(row).ok())
                .map(|mut price| {
                    price.currency.make_ascii_lowercase();
                    price
                })
                .filter(PlanPrice::valid)
                .collect(),
        ),
        _ => None,
    })
}
fn returning_until<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(match value {
        Some(serde_json::Value::String(text))
            if text.len() <= 64
                && time::OffsetDateTime::parse(
                    &text,
                    &time::format_description::well_known::Rfc3339,
                )
                .is_ok() =>
        {
            Some(text)
        }
        _ => None,
    })
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Device {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installation_id: Option<String>,
    pub name: String,
    pub last_seen: String,
    #[serde(rename = "this")]
    pub current: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HostKind {
    Ssh,
    Device,
    Worker,
    /// Never delivered to callers: `hosts()` and events drop such rows.
    #[serde(other)]
    Unknown,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HostStatus {
    Connected,
    Connecting,
    Prompting,
    Offline,
    /// Treated as not connected.
    #[serde(other)]
    Unknown,
}
// Intentionally no Debug: the daemon token must never appear in logs.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Daemon {
    pub token: String,
    pub build: String,
    pub sessions: usize,
}
impl std::fmt::Debug for Daemon {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Daemon")
            .field("token", &"[redacted]")
            .field("build", &self.build)
            .field("sessions", &self.sessions)
            .finish()
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Host {
    pub id: String,
    pub alias: String,
    pub kind: HostKind,
    pub status: HostStatus,
    pub daemon: Option<Daemon>,
    pub error: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AddHost {
    pub alias: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh: Option<SshTarget>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SshTarget {
    pub hostname: String,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default = "default_ssh_port")]
    pub port: u16,
}
fn default_ssh_port() -> u16 {
    22
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: String,
    pub token_type: String,
    pub expires_in: u64,
}
impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tokens")
            .field("access_token", &"[redacted]")
            .field("refresh_token", &"[redacted]")
            .field("expires_in", &self.expires_in)
            .finish()
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct TokenRequest {
    pub grant_type: String,
    pub code: String,
    pub redirect_uri: String,
    pub code_verifier: String,
    pub device_name: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct RefreshRequest {
    pub refresh_token: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Host {
        host: Host,
    },
    HostRemoved {
        host_id: String,
    },
    Prompt {
        id: String,
        host_id: String,
        prompt: String,
        echo: bool,
    },
    PromptClosed {
        id: String,
    },
    /// An event type this client does not know; ignored.
    #[serde(other)]
    Unknown,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventCommand {
    Answer { id: String, value: Option<String> },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServeCommand {
    Register { alias: String, daemon: Daemon },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServeEvent {
    Registered {
        host_id: String,
    },
    Open {
        stream_id: String,
    },
    Close {
        stream_id: String,
    },
    /// A control message this client does not know; ignored, never a reason
    /// to drop the whole reverse connection.
    #[serde(other)]
    Unknown,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApiError {
    pub error: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn account(extra: serde_json::Value) -> Account {
        let mut value = serde_json::json!({"account_id":"a","email":"a@example.invalid","plan":"pro","device_id":"d","protocol":0,"keeper_url":"","limits":{"cloud_hours":1,"storage_bytes":1},"usage":{"cloud_hours":0,"storage_bytes":0},"hours_exhausted":false});
        value
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        serde_json::from_value(value).unwrap()
    }
    #[test]
    fn an_ended_plans_return_window_is_optional_and_never_fails_the_account_read() {
        assert!(account(serde_json::json!({})).returning_until.is_none());
        assert!(account(serde_json::json!({"returning_until":null}))
            .returning_until
            .is_none());
        let ended =
            account(serde_json::json!({"plan":"none","returning_until":"2026-11-03T09:30:00Z"}));
        assert_eq!(
            ended.returning_until.as_deref(),
            Some("2026-11-03T09:30:00Z")
        );
        assert_eq!(
            serde_json::to_value(&ended).unwrap()["returning_until"],
            "2026-11-03T09:30:00Z"
        );
        // A value this client cannot read as a time is dropped; the account still reads.
        for unreadable in [
            serde_json::json!("soon"),
            serde_json::json!("2026-11-03"),
            serde_json::json!(1_780_000_000),
            serde_json::json!({"at":"2026-11-03T09:30:00Z"}),
            serde_json::json!("2026-11-03T09:30:00Z".repeat(8)),
        ] {
            let read = account(serde_json::json!({"returning_until":unreadable}));
            assert!(read.returning_until.is_none(), "{unreadable}");
            assert_eq!(read.email, "a@example.invalid");
        }
        // Absent stays absent on the wire: an older app never sees the key.
        assert!(serde_json::to_value(account(serde_json::json!({})))
            .unwrap()
            .get("returning_until")
            .is_none());
    }
    #[test]
    fn billing_extensions_are_optional_and_pricing_never_fails_the_account_read() {
        let older = account(serde_json::json!({}));
        assert!(!older.needs_payment() && older.plans.is_none());
        assert!(account(serde_json::json!({"payment_due":true})).needs_payment());
        assert!(account(serde_json::json!({"subscription_status":"past_due"})).needs_payment());
        assert!(!account(serde_json::json!({"subscription_status":"active"})).needs_payment());
        let offered = account(serde_json::json!({"plans":[
            {"plan":"pro","interval":"month","amount_cents":12345,"currency":"USD"},
            {"plan":"team","interval":"month","amount_cents":1,"currency":"usd"},
            {"plan":"max","interval":"fortnight","amount_cents":1,"currency":"usd"},
            {"plan":"max","interval":"year","amount_cents":"lots","currency":"usd"},
            {"plan":"none","interval":"year","amount_cents":0,"currency":"usd"},
            {"plan":"max","interval":"year","amount_cents":1,"currency":"dollars"}
        ]}));
        assert_eq!(
            offered.plans,
            Some(vec![PlanPrice {
                plan: Plan::Pro,
                interval: BillingInterval::Month,
                amount_cents: 12345,
                currency: "usd".into(),
                cloud_time_multiple: None,
                storage_multiple: None,
            }])
        );
        assert_eq!(account(serde_json::json!({"plans":{"pro":1}})).plans, None);
        assert_eq!(account(serde_json::json!({"plans":null})).plans, None);
    }
    fn offer(extra: serde_json::Value) -> PlanPrice {
        let mut row = serde_json::json!({
            "plan":"pro","interval":"month","amount_cents":111,"currency":"usd"
        });
        row.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let read = account(serde_json::json!({"plans":[row]}));
        let mut rows = read.plans.expect("the row is kept");
        assert_eq!(rows.len(), 1, "a multiple never drops a price row");
        rows.remove(0)
    }
    #[test]
    fn plan_multiples_are_kept_when_present_and_the_row_reads_without_them() {
        let both = offer(serde_json::json!({"cloud_time_multiple":5,"storage_multiple":3}));
        assert_eq!(
            (both.cloud_time_multiple, both.storage_multiple),
            (Some(5), Some(3))
        );
        assert_eq!(both.amount_cents, 111);
        // Older services omit both; a service may state only one.
        let neither = offer(serde_json::json!({}));
        assert_eq!(
            (neither.cloud_time_multiple, neither.storage_multiple),
            (None, None)
        );
        let time_only = offer(serde_json::json!({"cloud_time_multiple":4}));
        assert_eq!(
            (time_only.cloud_time_multiple, time_only.storage_multiple),
            (Some(4), None)
        );
        // Absent stays absent toward the page, so an older page sees no new key.
        let wire = serde_json::to_value(&neither).unwrap();
        assert!(
            wire.get("cloud_time_multiple").is_none() && wire.get("storage_multiple").is_none()
        );
        let wire = serde_json::to_value(&both).unwrap();
        assert_eq!(wire["cloud_time_multiple"], 5);
        assert_eq!(wire["storage_multiple"], 3);
        // The list states no absolute allowance, and none is passed through.
        let wire = serde_json::to_value(&both).unwrap();
        assert!(wire.get("cloud_hours").is_none() && wire.get("storage_bytes").is_none());
        let stray = offer(serde_json::json!({"cloud_hours":7,"storage_bytes":9000}));
        let wire = serde_json::to_value(&stray).unwrap();
        assert!(wire.get("cloud_hours").is_none() && wire.get("storage_bytes").is_none());
        // Pro's own entries carry 1.
        let one = offer(serde_json::json!({"cloud_time_multiple":1,"storage_multiple":1}));
        assert_eq!(
            (one.cloud_time_multiple, one.storage_multiple),
            (Some(1), Some(1))
        );
    }
    #[test]
    fn a_malformed_multiple_reads_as_absent_and_never_drops_the_row() {
        for bad in [
            serde_json::json!(0),
            serde_json::json!("lots"),
            serde_json::json!("5"),
            serde_json::json!(-1),
            serde_json::json!(2.5),
            serde_json::json!(null),
            serde_json::json!(true),
            serde_json::json!([1]),
            serde_json::json!(u64::from(u32::MAX) + 1),
            serde_json::json!(u64::MAX),
        ] {
            let read = offer(serde_json::json!({"cloud_time_multiple":bad,"storage_multiple":bad}));
            assert_eq!(
                (read.cloud_time_multiple, read.storage_multiple),
                (None, None),
                "{bad}"
            );
            assert_eq!(read.amount_cents, 111, "{bad}");
        }
        // One bad field leaves the other intact.
        let mixed = offer(serde_json::json!({"cloud_time_multiple":"lots","storage_multiple":5}));
        assert_eq!(
            (mixed.cloud_time_multiple, mixed.storage_multiple),
            (None, Some(5))
        );
        let edge = offer(serde_json::json!({"cloud_time_multiple":u32::MAX}));
        assert_eq!(edge.cloud_time_multiple, Some(u32::MAX));
    }
    #[test]
    fn the_public_catalog_carries_multiples_like_the_account_offers() {
        let rows = serde_json::json!([
            {"plan":"pro","interval":"month","amount_cents":111,"currency":"usd",
             "cloud_time_multiple":1,"storage_multiple":1},
            {"plan":"max","interval":"year","amount_cents":1110,"currency":"usd",
             "cloud_time_multiple":4,"storage_multiple":"lots"}
        ]);
        let catalog: PlanCatalog =
            serde_json::from_value(serde_json::json!({"plans": rows})).unwrap();
        assert_eq!(
            catalog.plans,
            account(serde_json::json!({"plans": rows})).plans
        );
        let plans = catalog.plans.unwrap();
        assert_eq!(plans[0].cloud_time_multiple, Some(1));
        assert_eq!(plans[0].storage_multiple, Some(1));
        assert_eq!(plans[1].cloud_time_multiple, Some(4));
        assert_eq!(plans[1].storage_multiple, None);
    }
    #[test]
    fn the_public_catalog_reads_exactly_like_the_account_offers() {
        let rows = serde_json::json!([
            {"plan":"pro","interval":"month","amount_cents":111,"currency":"USD"},
            {"plan":"team","interval":"month","amount_cents":1,"currency":"usd"},
            {"plan":"max","interval":"year","amount_cents":"lots","currency":"usd"}
        ]);
        let catalog: PlanCatalog =
            serde_json::from_value(serde_json::json!({"plans": rows})).unwrap();
        assert_eq!(
            catalog.plans,
            account(serde_json::json!({"plans": rows})).plans
        );
        assert_eq!(catalog.plans.as_ref().map(Vec::len), Some(1));
        // No catalog yet, a missing key and a malformed list are all "no offers".
        for none in [
            serde_json::json!({"plans":null}),
            serde_json::json!({}),
            serde_json::json!({"plans":{"pro":1}}),
        ] {
            let catalog: PlanCatalog = serde_json::from_value(none).unwrap();
            assert_eq!(catalog.plans, None);
        }
    }
}
