use std::fs::{File, OpenOptions};

/// Keep this file in place: replacing its inode would split the lock domain.
#[cfg_attr(feature = "addon", napi_derive::napi)]
pub struct WriterMutationLock {
    file: Option<File>,
}

#[cfg_attr(feature = "addon", napi_derive::napi)]
impl WriterMutationLock {
    #[cfg_attr(feature = "addon", napi)]
    pub fn release(&mut self) {
        self.file.take();
    }
}

/// OS locks release on close or process exit, so a crashed writer cannot
/// leave a stale coordination lock that another process must delete.
#[cfg_attr(feature = "addon", napi_derive::napi)]
pub fn try_writer_mutation_lock(path: String) -> crate::boundary::Result<Option<WriterMutationLock>> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|e| crate::boundary::Error::from_reason(e.to_string()))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(WriterMutationLock { file: Some(file) })),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(e)) => Err(crate::boundary::Error::from_reason(e.to_string())),
    }
}
