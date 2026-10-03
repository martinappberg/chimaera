//! Opt-in fixture authority only. No SSH or local key operations occur here.
use crate::*;
use std::{collections::HashMap, time::Duration};
use tokio::{sync::Mutex, time::Instant};
#[derive(Default)]
pub(crate) struct FixtureSshAuth(Mutex<Data>);
#[derive(Default)]
struct Data {
    enabled: bool,
    boot: String,
    targets: HashMap<String, SshAuthDestination>,
    grants: HashMap<String, Grant>,
    reconnects: usize,
}
struct Grant {
    host: String,
    device: String,
    epoch: u64,
    boot: String,
    destination: SshAuthDestination,
    deadline: Instant,
    live: bool,
    reserved: bool,
}
impl FixtureSshAuth {
    pub async fn enabled(&self, enabled: bool) {
        let mut data = self.0.lock().await;
        data.enabled = enabled;
        data.boot = nonce();
        data.grants.clear();
    }
    pub async fn capabilities(&self) -> Option<SshAuthCapabilities> {
        let data = self.0.lock().await;
        data.enabled.then(|| SshAuthCapabilities {
            version: 1,
            hostbound_v1: true,
            register_only_v1: true,
            proxyjump_v1: false,
            keeper_boot: data.boot.clone(),
        })
    }
    pub async fn target(
        &self,
        host: &str,
        target: SshAuthDestination,
    ) -> Result<(), axum::http::StatusCode> {
        target
            .validate()
            .map_err(|_| axum::http::StatusCode::BAD_REQUEST)?;
        let mut data = self.0.lock().await;
        if !data.targets.contains_key(host) && data.targets.len() >= MAX_STREAMS {
            return Err(axum::http::StatusCode::TOO_MANY_REQUESTS);
        }
        data.targets.insert(host.into(), target);
        Ok(())
    }
    pub async fn matches_target(&self, host: &str, target: &SshAuthDestination) -> bool {
        self.0.lock().await.targets.get(host) == Some(target)
    }
    pub async fn expire(&self, id: &str) {
        if let Some(grant) = self.0.lock().await.grants.get_mut(id) {
            grant.deadline = Instant::now();
        }
    }
    pub async fn remove(&self, host: &str) {
        let mut data = self.0.lock().await;
        data.targets.remove(host);
        data.grants.retain(|_, grant| grant.host != host);
    }
    pub async fn issue(
        &self,
        host: &str,
        device: &str,
        epoch: u64,
        request: SshAuthGrantRequest,
    ) -> Result<SshAuthGrant, axum::http::StatusCode> {
        use axum::http::StatusCode;
        request.validate().map_err(|_| StatusCode::BAD_REQUEST)?;
        let mut data = self.0.lock().await;
        if !data.enabled {
            return Err(StatusCode::NOT_FOUND);
        }
        if data.boot != request.keeper_boot || data.targets.get(host) != Some(&request.destination)
        {
            return Err(StatusCode::CONFLICT);
        }
        data.grants
            .retain(|_, grant| grant.deadline > Instant::now());
        if data.grants.len() >= 32
            || data
                .grants
                .values()
                .filter(|grant| grant.device == device)
                .count()
                >= 4
        {
            return Err(StatusCode::TOO_MANY_REQUESTS);
        }
        let id = nonce();
        let boot = data.boot.clone();
        data.grants.insert(
            id.clone(),
            Grant {
                host: host.into(),
                device: device.into(),
                epoch,
                boot,
                destination: request.destination,
                deadline: Instant::now() + Duration::from_secs(SSH_AUTH_LIFETIME.into()),
                live: false,
                reserved: false,
            },
        );
        Ok(SshAuthGrant {
            version: 1,
            grant_id: id,
            expires_in: SSH_AUTH_LIFETIME,
        })
    }
    fn valid(data: &Data, grant: &Grant, host: &str, device: &str, epoch: u64) -> bool {
        data.enabled
            && grant.host == host
            && grant.device == device
            && grant.epoch == epoch
            && grant.boot == data.boot
            && grant.deadline > Instant::now()
            && data.targets.get(host) == Some(&grant.destination)
    }
    pub async fn attach(
        &self,
        host: &str,
        id: &str,
        device: &str,
        epoch: u64,
    ) -> Result<(), axum::http::StatusCode> {
        use axum::http::StatusCode;
        let mut data = self.0.lock().await;
        let grant = data.grants.get(id).ok_or(StatusCode::NOT_FOUND)?;
        if !Self::valid(&data, grant, host, device, epoch) {
            return Err(StatusCode::FORBIDDEN);
        }
        if grant.reserved {
            return Err(StatusCode::CONFLICT);
        }
        data.grants.get_mut(id).unwrap().reserved = true;
        Ok(())
    }
    pub async fn activate(
        &self,
        host: &str,
        id: &str,
        device: &str,
        epoch: u64,
    ) -> Result<SshAuthHello, axum::http::StatusCode> {
        let mut data = self.0.lock().await;
        let grant = data
            .grants
            .get(id)
            .ok_or(axum::http::StatusCode::NOT_FOUND)?;
        if !grant.reserved || grant.live || !Self::valid(&data, grant, host, device, epoch) {
            return Err(axum::http::StatusCode::FORBIDDEN);
        }
        let boot = grant.boot.clone();
        data.grants.get_mut(id).unwrap().live = true;
        Ok(SshAuthHello::Ready {
            version: 1,
            grant_id: id.into(),
            keeper_boot: boot,
        })
    }
    pub async fn live(&self, host: &str, id: &str, device: &str, epoch: u64) -> bool {
        let data = self.0.lock().await;
        data.grants
            .get(id)
            .is_some_and(|grant| grant.live && Self::valid(&data, grant, host, device, epoch))
    }
    pub async fn detach(&self, id: &str) {
        // Channel loss is terminal for this grant, never an implicit replacement.
        self.0.lock().await.grants.remove(id);
    }
    pub async fn delete(
        &self,
        host: &str,
        id: &str,
        device: &str,
        epoch: u64,
    ) -> Result<(), axum::http::StatusCode> {
        let mut data = self.0.lock().await;
        if let Some(grant) = data.grants.get(id) {
            if grant.host != host || grant.device != device || grant.epoch != epoch {
                return Err(axum::http::StatusCode::FORBIDDEN);
            }
        }
        data.grants.remove(id);
        Ok(())
    }
    pub async fn reconnect(
        &self,
        host: &str,
        id: &str,
        device: &str,
        epoch: u64,
    ) -> Result<(), axum::http::StatusCode> {
        let mut data = self.0.lock().await;
        if !data
            .grants
            .get(id)
            .is_some_and(|grant| grant.live && Self::valid(&data, grant, host, device, epoch))
        {
            return Err(axum::http::StatusCode::FORBIDDEN);
        }
        data.reconnects = data.reconnects.saturating_add(1);
        Ok(())
    }
    pub async fn reconnect_count(&self) -> usize {
        self.0.lock().await.reconnects
    }
}
fn nonce() -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>())
}
