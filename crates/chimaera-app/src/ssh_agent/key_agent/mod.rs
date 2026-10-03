//! Original-Connect private agent ownership. No key, socket or loader command
//! is accepted from the keeper; configured-file admission lives in selection.
mod add;
mod backend;
mod files;
mod load;
mod process;

pub(crate) fn run_helper() -> i32 {
    process::run_helper()
}

pub(crate) fn loaded_public(
    frame: &zeroize::Zeroizing<Vec<u8>>,
    lifetime: u32,
) -> Result<Vec<u8>, super::selection::SelectionFailure> {
    add::public_identity(frame, lifetime)
}

pub(crate) use backend::Agent;
pub(crate) use load::{CertificateFile, SelectedFile};
#[derive(Clone)]
pub(crate) struct Admission(process::Admission);
impl Admission {
    pub(crate) fn acquire() -> Result<Self, super::selection::SelectionFailure> {
        Ok(Self(process::Admission::acquire()?))
    }
}
#[derive(Clone)]
pub(crate) struct Session(process::Lease);
impl Session {
    pub(crate) async fn spawn(
        owner: &super::trust::Owner,
        deadline: tokio::time::Instant,
        admission: &Admission,
    ) -> Result<Self, super::selection::SelectionFailure> {
        Ok(Self(
            process::Lease::spawn(owner, deadline, &admission.0).await?,
        ))
    }
    pub(crate) async fn load(
        &self,
        file: &SelectedFile,
        owner: &super::trust::Owner,
        deadline: tokio::time::Instant,
    ) -> Result<Vec<u8>, super::selection::SelectionFailure> {
        load::load(file, &self.0, owner, deadline).await
    }
}
