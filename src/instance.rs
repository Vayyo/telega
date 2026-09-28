//! One client per data directory.
//!
//! A launcher entry is easy to trigger twice, and two clients on one data
//! directory start two TDLib databases in the same place, which TDLib does
//! not share: the second window would only show errors. The first client
//! holds an exclusive lock on `<data>/telega.lock`; a later one leaves the
//! directory alone and exits. The kernel drops the lock when the process
//! ends, however it ends, so a crash does not lock the user out.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::Path;

/// What became of the request for the data directory.
pub enum Lock {
    /// This process owns the directory; the guard lives as long as the client.
    Held(Guard),
    /// Another client is already running on this directory.
    Busy,
}

/// Keeps the data directory locked until it is dropped.
pub struct Guard {
    _file: File,
}

/// Takes the lock of the data directory. Call before TDLib starts.
pub fn acquire() -> Result<Lock, String> {
    acquire_at(&crate::paths::base().join("telega.lock"))
}

fn acquire_at(path: &Path) -> Result<Lock, String> {
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)
        .map_err(|e| {
            format!(
                "не удалось открыть файл блокировки {}: {e}. Проверьте доступ к каталогу данных",
                path.display()
            )
        })?;
    match file.try_lock() {
        Ok(()) => Ok(Lock::Held(Guard { _file: file })),
        Err(TryLockError::WouldBlock) => Ok(Lock::Busy),
        Err(TryLockError::Error(e)) => Err(format!(
            "не удалось заблокировать файл {}: {e}. Проверьте поддержку файловых блокировок",
            path.display()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held(lock: Result<Lock, String>) -> Guard {
        match lock.expect("блокировка доступна") {
            Lock::Held(guard) => guard,
            Lock::Busy => panic!("каталог занят, хотя клиента нет"),
        }
    }

    #[test]
    fn a_second_client_is_refused_and_the_lock_frees_on_drop() {
        // The lock file lives in the data directory: without it `acquire`
        // cannot lock anything (as `main` would find out too).
        crate::paths::init().expect("каталог данных для теста");
        let first = held(acquire());
        assert!(
            matches!(acquire(), Ok(Lock::Busy)),
            "второй клиент должен увидеть занятый каталог"
        );
        drop(first);
        let _second = held(acquire());
    }

    #[test]
    fn unusable_lock_path_refuses_to_start() {
        crate::paths::init().expect("каталог данных для теста");
        // Opening a directory for writing fails without changing any files.
        let path = crate::paths::base();
        let error = match acquire_at(path) {
            Err(error) => error,
            Ok(_) => panic!("каталог не может быть файлом блокировки"),
        };
        assert!(error.contains("не удалось открыть файл блокировки"));
        assert!(error.contains(&path.display().to_string()));
    }
}
