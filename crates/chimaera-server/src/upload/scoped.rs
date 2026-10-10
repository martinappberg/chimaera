//! Scoped uploads stream into an owned hidden file, then publish under the
//! exact admitted epoch. Cancellation retains the descriptor and work slot
//! until bounded cleanup has drained; it never reopens the temporary by path.
use super::*;
use crate::workspace_scope::files::{Context as Files, Entry};
use rustix::fs::{AtFlags, OFlags, RenameFlags};
use std::os::unix::fs::MetadataExt;

struct Pending {
    entry: Option<Entry>,
    inode: u64,
    permit: Option<tokio::sync::SemaphorePermit<'static>>,
    done: Option<tokio::sync::oneshot::Sender<()>>,
}
impl Drop for Pending {
    fn drop(&mut self) {
        let Some(entry) = self.entry.take() else {
            return;
        };
        let inode = self.inode;
        let permit = self.permit.take();
        let done = self.done.take();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            if rustix::fs::statat(&entry.directory, &entry.name, AtFlags::SYMLINK_NOFOLLOW)
                .is_ok_and(|stat| stat.st_ino == inode)
            {
                let _ = rustix::fs::unlinkat(&entry.directory, &entry.name, AtFlags::empty());
            }
            drop(_permit);
            if let Some(done) = done {
                let _ = done.send(());
            }
        });
    }
}
pub(super) async fn upload(
    state: Arc<AppState>,
    filesystem: Files,
    mutation: Option<Extension<crate::workspace_scope::Mutation>>,
    dir: String,
    name: String,
    body: Body,
) -> Response {
    upload_with_limits(
        state,
        filesystem,
        mutation,
        dir,
        name,
        body,
        std::time::Duration::from_secs(60),
        std::time::Duration::from_secs(30 * 60),
    )
    .await
}
#[allow(clippy::too_many_arguments)]
pub async fn upload_with_limits(
    state: Arc<AppState>,
    filesystem: Files,
    mutation: Option<Extension<crate::workspace_scope::Mutation>>,
    dir: String,
    name: String,
    body: Body,
    idle: std::time::Duration,
    total: std::time::Duration,
) -> Response {
    let Ok(permit) = crate::fs::FILESYSTEM_WORK.acquire().await else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    // Admission is bounded before detaching. The owned stream retains its
    // file and work slot through an in-flight Tokio file write after cancel.
    let operation = tokio::spawn(async move {
        let observed = filesystem.clone();
        let result = upload_inner(
            &state, filesystem, mutation, &dir, name, body, permit, idle, total,
        )
        .await;
        match result {
            Ok(value) => {
                crate::git::mark_path_dirty(&state, &dir).await;
                Json(value).into_response()
            }
            Err(error) => {
                let error = observed.validate().err().unwrap_or(error);
                crate::workspace_scope::mutation_failure(&error)
                    .unwrap_or_else(|| crate::fs::bad_request(&error))
            }
        }
    });
    operation
        .await
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}
#[allow(clippy::too_many_arguments)]
async fn upload_inner(
    state: &Arc<AppState>,
    filesystem: Files,
    mutation: Option<Extension<crate::workspace_scope::Mutation>>,
    dir: &str,
    name: String,
    body: Body,
    permit: tokio::sync::SemaphorePermit<'static>,
    idle: std::time::Duration,
    total: std::time::Duration,
) -> anyhow::Result<serde_json::Value> {
    let (done, drained) = tokio::sync::oneshot::channel();
    let outcome = async {
    let temporary = Path::new(dir).join(crate::persist::project_temp_name(std::ffi::OsStr::new(&name)));
    let scope = filesystem.clone();
    let (mut pending, file) = tokio::task::spawn_blocking(move || {
        let entry = scope.entry(&temporary.to_string_lossy(), false, false)?;
        let file = entry.open(OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL)?;
        let inode = file.metadata()?.ino();
        anyhow::Ok((Pending { entry: Some(entry), inode, permit: Some(permit), done: Some(done) }, file))
    }).await??;
    let mut file = tokio::fs::File::from_std(file);
    let mut written = 0_u64;
    let mut stream = body.into_data_stream();
    let deadline = tokio::time::Instant::now() + total;
    loop {
        let next = tokio::time::timeout_at(deadline.min(tokio::time::Instant::now() + idle), stream.next())
            .await.map_err(|_|anyhow::anyhow!("upload body exceeded idle or total time limit"))?;
        let Some(chunk) = next else { break; };
        let chunk = chunk?;
        written = written.checked_add(chunk.len() as u64).ok_or_else(||anyhow::anyhow!("upload size overflow"))?;
        anyhow::ensure!(written <= MAX_DIR_UPLOAD_BYTES, "upload exceeds file size limit");
        pending = tokio::task::spawn_blocking(move || {
            pending.entry.as_ref().expect("pending entry").current()?;
            anyhow::Ok(pending)
        }).await??;
        file.write_all(&chunk).await?;
    }
    anyhow::ensure!(tokio::time::Instant::now() < deadline, "upload body exceeded total time limit");
    file.flush().await?;
    file.sync_all().await?;
    drop(file);
    let owner = state.clone();
    let directory = PathBuf::from(dir);
    tokio::task::spawn_blocking(move || {
        let _commit = crate::workspace_scope::begin_mutation(&owner, &mutation)?;
        let source = pending.entry.as_ref().expect("pending entry");
        source.check()?;
        anyhow::ensure!(source.stat()?.is_some_and(|stat|stat.st_ino == pending.inode), "upload temporary changed");
        let (stem, extension) = name.split_once('.').map(|(stem, extension)|(stem.to_owned(),format!(".{extension}"))).unwrap_or((name.clone(),String::new()));
        for index in 0..10_000 {
            let final_name = match index { 0 => name.clone(), 1 => format!("{stem} copy{extension}"), _ => format!("{stem} copy {index}{extension}") };
            let target = filesystem.entry(&directory.join(&final_name).to_string_lossy(), false, false)?;
            source.check()?; target.check()?;
            match rustix::fs::renameat_with(&source.directory, &source.name, &target.directory, &target.name, RenameFlags::NOREPLACE) {
                Ok(()) => return Ok(json!({"path":target.canonical.to_string_lossy(),"name":final_name,"size":written})),
                Err(rustix::io::Errno::EXIST) => continue,
                Err(error) => return Err(error.into()),
            }
        }
        anyhow::bail!("no free copy name available")
    }).await?
    }.await;
    let _ = drained.await;
    outcome
}
