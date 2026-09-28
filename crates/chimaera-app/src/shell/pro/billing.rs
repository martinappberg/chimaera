//! Billing stays in the system browser; device credentials never enter the UI.
use super::super::Shell;
use chimaera_link::{BillingInterval, Plan};
use tauri::{AppHandle, Manager};

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
    checkout: Option<(Plan, BillingInterval)>,
) -> Result<(), String> {
    let state = app.state::<Shell>();
    let (client, generation) = state
        .pro
        .client_snapshot()
        .await
        .ok_or("Sign in before opening billing.")?;
    let session = if let Some((plan, interval)) = checkout {
        if plan == Plan::None {
            return Err("Choose Pro or Max.".into());
        }
        client.billing_checkout(plan, interval).await
    } else {
        client.billing_portal().await
    }
    .map_err(|_| "Couldn't open billing. Check your connection and try again.".to_string())?;
    let url = billing_url(&session.url)?;
    let _operation = state.pro.operation.lock().await;
    if state.pro.generation() != generation {
        return Err("Your account changed. Open billing again.".into());
    }
    tokio::task::spawn_blocking(move || open::that(url.as_str()))
        .await
        .map_err(|_| "Couldn't open your browser. Try again.".to_string())?
        .map_err(|_| "Couldn't open your browser. Try again.".into())
}

#[tauri::command]
pub async fn pro_billing_checkout(
    app: AppHandle,
    plan: Plan,
    interval: BillingInterval,
) -> Result<(), String> {
    open_billing(app, Some((plan, interval))).await
}

#[tauri::command]
pub async fn pro_billing_portal(app: AppHandle) -> Result<(), String> {
    open_billing(app, None).await
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
}
