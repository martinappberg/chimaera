//! Fixed internal portable-configuration companion contract. Not an HTTP API.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
pub(super) const VERSION: u16 = 1;
pub(super) const REQUEST_CAP: usize = 16 * 1024;
pub(super) const REPLY_CAP: usize = 32 * 1024;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Request {
    pub version: u16,
    pub operation: Operation,
    pub workspace: PathBuf,
    pub budget: u64,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Operation {
    Export {
        home: i32,
        claude: Option<i32>,
        codex: Option<i32>,
        output: i32,
    },
    MergeStaged {
        overlay: i32,
        after: i32,
    },
}
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub files: usize,
    pub bytes: u64,
    pub excluded: usize,
    pub missing_environment: Vec<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Reply {
    pub version: u16,
    pub report: Report,
}
pub(super) fn valid_request(request: &Request) -> bool {
    let descriptors = match request.operation {
        Operation::Export {
            home,
            claude,
            codex,
            output,
        } => [Some(home), claude, codex, Some(output)],
        Operation::MergeStaged { overlay, after } => [Some(overlay), Some(after), None, None],
    };
    request.version == VERSION
        && request.workspace.is_absolute()
        && request.workspace.as_os_str().len() <= 8192
        && !request
            .workspace
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
        && request.budget <= 512 * 1024 * 1024
        && descriptors
            .iter()
            .flatten()
            .all(|fd| (3..=65_535).contains(fd))
        && descriptors
            .iter()
            .enumerate()
            .all(|(i, fd)| fd.is_none() || !descriptors[..i].contains(fd))
}
pub(super) fn valid_report(report: &Report, budget: u64) -> bool {
    report.files <= 32_768
        && report.excluded <= 32_768
        && report.bytes <= budget
        && report.missing_environment.len() <= 128
        && report.missing_environment.iter().all(|name| {
            !name.is_empty()
                && name.len() <= 128
                && name.bytes().enumerate().all(|(i, b)| {
                    b == b'_' || b.is_ascii_alphabetic() || i > 0 && b.is_ascii_digit()
                })
        })
}
