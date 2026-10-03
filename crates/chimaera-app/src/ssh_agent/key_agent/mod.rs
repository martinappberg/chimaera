//! Original-Connect private agent ownership. No key, socket or loader command
//! is accepted from the keeper; configured-file admission lives in selection.
mod add;
mod files;
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
