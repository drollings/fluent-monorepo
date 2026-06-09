use fs2::FileExt;
use fluent_wvr_common::hash::blake3_hex;
use std::fs::{self, File};
use std::path::Path;

pub struct FileLock {
    lock_path: String,
    file: Option<File>,
    pub acquired: bool,
}

impl FileLock {
    pub fn acquire(source_file: &str) -> std::io::Result<Self> {
        let hash = blake3_hex(source_file.as_bytes());
        let lock_dir = Path::new(source_file)
            .parent()
            .unwrap_or(Path::new("."))
            .join(".guidance")
            .join("locks");
        fs::create_dir_all(&lock_dir)?;
        let lock_path = lock_dir.join(format!("{}.lock", &hash[..16]));
        let lock_path_str = lock_path.to_string_lossy().to_string();
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&lock_path)?;
        match file.try_lock_exclusive() {
            Ok(()) => Ok(Self {
                lock_path: lock_path_str,
                file: Some(file),
                acquired: true,
            }),
            Err(_) => Ok(Self {
                lock_path: lock_path_str,
                file: Some(file),
                acquired: false,
            }),
        }
    }

    pub fn release(&mut self) {
        if let Some(ref file) = self.file {
            let _ = file.unlock();
        }
        let _ = fs::remove_file(&self.lock_path);
        self.acquired = false;
        self.file = None;
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        if self.acquired {
            self.release();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acquire_and_release() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("test.txt");
        fs::write(&source, "test").unwrap();
        let mut lock = FileLock::acquire(&source.to_string_lossy()).unwrap();
        assert!(lock.acquired);
        lock.release();
    }
}
