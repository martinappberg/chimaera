//! Stable native account presentation shapes. No credential or transport owner lives here.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
pub mod account {
    use super::*;
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
    #[serde(rename_all = "snake_case")]
    pub enum InitializationPhase {
        Keychain,
        Account,
        Connection,
    }

    #[derive(Serialize)]
    pub struct Status {
        /// Original process/account owner receipt, stable across ordinary refresh.
        pub account_lifetime: Option<String>,
        /// Only the successful original browser sign-in may preserve its UI intent.
        pub completed_sign_in_lifetime: Option<String>,
        pub initializing: bool,
        pub initialization_phase: Option<InitializationPhase>,
        pub available: bool,
        pub signed_in: bool,
        pub email: Option<String>,
        pub plan: Option<chimaera_link::Plan>,
        /// A failure that needs the user (sign in again, unlock the credential
        /// store) or a fixed code the UI maps to copy. Never informational.
        pub error: Option<String>,
        /// Additive: informational fixed code (see `code`); work continues and
        /// Chimaera retries on its own. Never shown as a failure or used to
        /// decide plan branding.
        pub connection_warning: Option<&'static str>,
        /// Additive: the subscription needs a payment update.
        pub payment_due: bool,
        /// Additive: the plan has ended and this RFC 3339 time is how long its
        /// cloud work can still be brought home; null otherwise.
        pub returning_until: Option<String>,
        /// Additive: the RFC 3339 time the always-on cloud connection restarts to
        /// update (a past time: shortly, once no Git transfer runs), dropping the
        /// cluster logins it holds; null when none is planned.
        pub keeper_restart_at: Option<String>,
        /// Additive: the offers to display, passed through when the service
        /// supplies them; clients never hardcode prices. A signed-in account's own
        /// list wins; otherwise the service's public catalog (`GET /v1/plans`),
        /// which is how a signed-out page shows prices.
        pub plans: Option<Vec<chimaera_link::PlanPrice>>,
        pub sign_in: Option<super::auth::Status>,
        pub billing: Option<super::billing::Status>,
        pub limits: Option<chimaera_link::Limits>,
        pub usage: Option<chimaera_link::Usage>,
        pub hours_exhausted: bool,
    }

    #[derive(Serialize)]
    pub struct KeptHost {
        pub alias: String,
        pub kept: bool,
        pub status: String,
        pub kind: String,
    }
}
pub mod auth {
    use super::*;
    #[derive(Clone, Copy, Default, Deserialize)]
    #[serde(rename_all = "kebab-case")]
    pub enum ScreenHint {
        SignUp,
        #[default]
        SignIn,
    }

    #[derive(Clone, Copy, Serialize, PartialEq, Eq)]
    #[serde(rename_all = "snake_case")]
    pub enum Phase {
        Waiting,
        Finishing,
    }

    #[derive(Clone, Serialize)]
    pub struct Status {
        pub phase: Phase,
        pub expires_at: u64,
    }
}
pub mod billing {
    use super::*;
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
    #[serde(rename_all = "snake_case")]
    pub enum Kind {
        Checkout,
        Portal,
        PlanChange,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
    #[serde(rename_all = "snake_case")]
    pub enum Phase {
        Opening,
        Waiting,
        Confirming,
        Confirmed,
        Unconfirmed,
        Canceled,
        Expired,
        Failed,
    }

    #[derive(Clone, Serialize)]
    pub struct Status {
        pub id: u64,
        pub kind: Kind,
        pub requested_plan: Option<chimaera_link::Plan>,
        pub phase: Phase,
        pub expires_at: u64,
        pub error: Option<String>,
    }
}
pub mod projects {
    use super::*;
    #[derive(Clone, Deserialize, Serialize)]
    pub struct CloudProject {
        pub workspace_id: String,
        pub name: String,
        #[serde(default)]
        pub host_id: Option<String>,
        #[serde(default)]
        pub host_alias: Option<String>,
        pub local_root: Option<PathBuf>,
        #[serde(default)]
        pub destination_saved: bool,
        pub available: bool,
        pub error: Option<String>,
    }

    #[derive(Deserialize, Serialize)]
    pub struct CloudProjectOpen {
        pub workspace_id: String,
        pub root: String,
        pub name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub local_copy: Option<serde_json::Value>,
    }
}
pub mod agents {
    use super::*;
    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    pub struct Row {
        pub id: String,
        pub label: String,
        pub category: String,
        pub state: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub methods: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub disconnect_supported: Option<bool>,
    }
}
pub mod cloud {
    use super::*;
    #[derive(Serialize)]
    pub struct CloudStatus {
        #[serde(flatten)]
        pub worker: chimaera_link::WorkerStatus,
        /// Additive: whether an agent was connected in the cloud at the last
        /// provider catalog read. Remembered, never probed, so a sleeping cloud
        /// is not woken to answer it. Absent when unknown.
        #[serde(skip_serializing_if = "Option::is_none")]
        pub agents_connected: Option<bool>,
        /// Additive: this account's cloud has been ready before, so `preparing`
        /// now (a service update, say) is not its first setup and the page keeps
        /// the calm available state. Remembered per account.
        pub cloud_ready_once: bool,
        /// Additive: the provider rows of the last catalog read, which the page
        /// shows at once and replaces when a live read answers. Absent when none
        /// are remembered.
        #[serde(skip_serializing_if = "Vec::is_empty")]
        pub remembered_providers: Vec<super::agents::Row>,
    }

    #[derive(Deserialize)]
    #[serde(tag = "operation", rename_all = "snake_case")]
    pub enum Request {
        Info,
        Start,
        Providers,
        ProviderConnect {
            provider_id: String,
        },
        ProviderDisconnect {
            provider_id: String,
            acknowledge_cloud_work: bool,
        },
        ProviderConnection {
            connection_id: String,
        },
        ProviderCancel {
            connection_id: String,
        },
        ProviderSubmit {
            connection_id: String,
            code: String,
        },
        OpenProviderBrowser {
            connection_id: String,
        },
        ResumeHandoff {
            workspace_id: String,
            expected_epoch: u64,
        },
        Project {
            url: String,
            name: Option<String>,
        },
    }
}

impl account::Status {
    pub fn absent() -> Self {
        Self {
            account_lifetime: None,
            completed_sign_in_lifetime: None,
            initializing: false,
            initialization_phase: None,
            available: false,
            signed_in: false,
            email: None,
            plan: None,
            error: None,
            connection_warning: None,
            payment_due: false,
            returning_until: None,
            keeper_restart_at: None,
            plans: None,
            sign_in: None,
            billing: None,
            limits: None,
            usage: None,
            hours_exhausted: false,
        }
    }
}

