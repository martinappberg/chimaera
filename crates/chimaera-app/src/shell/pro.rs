//! Optional typed account host adapters. No credentials, Client or account tasks here.
use super::Shell;
pub(super) fn reconfigure(app: &tauri::AppHandle) {
    use tauri::Manager;
    if let Some(owner) = app.state::<Shell>().pro.owner() {
        owner.reconfigure(app.clone());
    }
}
pub(super) fn direct_ssh_bypass(direct: bool, device: bool) -> Result<bool, String> {
    if direct && device {
        Err("Device connections cannot use Direct SSH".into())
    } else {
        Ok(direct)
    }
}
