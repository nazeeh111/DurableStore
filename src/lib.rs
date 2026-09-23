//! A single-writer embedded byte store with synchronized append-only records.
//!
//! A successful mutation has been synchronized to the filesystem. Recovery may
//! also retain complete unacknowledged records. This is not a transaction engine.
#![forbid(unsafe_code)]
#[cfg(not(unix))]
compile_error!("DurableStore currently supports local Unix filesystems (Linux/macOS) only");
mod format;
pub use format::{MAX_KEY, MAX_VALUE};
use std::{
    collections::BTreeMap,
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

pub type Result<T> = std::result::Result<T, Error>;
#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Locked,
    InvalidInput(&'static str),
    Corruption { offset: u64, reason: &'static str },
    Poisoned,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::Locked => write!(f, "store is locked by another handle"),
            Self::InvalidInput(e) => write!(f, "invalid input: {e}"),
            Self::Corruption { offset, reason } => {
                write!(f, "corruption at byte {offset}: {reason}")
            }
            Self::Poisoned => write!(
                f,
                "handle requires reopening after a failed persistence operation"
            ),
        }
    }
}
impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        if let Self::Io(e) = self {
            Some(e)
        } else {
            None
        }
    }
}
impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stats {
    pub live_keys: usize,
    pub live_bytes: u64,
    pub records: u64,
    pub log_bytes: u64,
    /// Bytes discarded on this handle's open, or found incomplete by inspect.
    pub tail_bytes: u64,
}

/// Owns the exclusive lock. Dropping closes the handle and releases the lock.
pub struct Store {
    dir: PathBuf,
    _lock: File,
    log: File,
    entries: BTreeMap<Vec<u8>, Vec<u8>>,
    records: u64,
    bytes: u64,
    recovered_tail: u64,
    poisoned: bool,
}

fn lock(dir: &Path, create: bool) -> Result<File> {
    if !dir.is_dir() {
        return Err(Error::InvalidInput("store directory must already exist"));
    }
    let f = OpenOptions::new()
        .read(true)
        .write(true)
        .create(create)
        .truncate(false)
        .open(dir.join("LOCK"))?;
    match f.try_lock() {
        Ok(()) => Ok(f),
        Err(std::fs::TryLockError::WouldBlock) => Err(Error::Locked),
        Err(std::fs::TryLockError::Error(e)) => Err(e.into()),
    }
}
fn sync_dir(dir: &Path) -> Result<()> {
    File::open(dir)?.sync_all()?;
    Ok(())
}

// The default build has no environment-controlled failure behavior.
fn fault(point: &str) -> Result<()> {
    #[cfg(feature = "fault-injection")]
    if std::env::var("DURABLESTORE_FAIL_AT").as_deref() == Ok(point) {
        if std::env::var("DURABLESTORE_FAIL_MODE").as_deref() == Ok("error") {
            return Err(io::Error::other(format!("injected failure at {point}")).into());
        }
        std::process::exit(86); // No Drop handlers, deliberately like abrupt termination.
    }
    let _ = point;
    Ok(())
}
impl Store {
    /// Open or initialize a store in an existing directory, recovering only an incomplete tail.
    pub fn open(dir: impl AsRef<Path>) -> Result<Self> {
        // Anchor future compactions even if the caller later changes process cwd.
        let dir = fs::canonicalize(dir.as_ref())?;
        let guard = lock(&dir, true)?;
        let log_path = dir.join("data.wal");
        if !log_path.exists() {
            let temp = dir.join("compact.tmp");
            let mut f = File::create(&temp)?;
            f.write_all(&format::file_header())?;
            fault("init_header")?;
            f.sync_all()?;
            fault("init_sync")?;
            fs::rename(&temp, &log_path)?;
            fault("init_rename")?;
            sync_dir(&dir)?;
            fault("init_dirsync")?;
        }
        let mut log = OpenOptions::new().read(true).write(true).open(log_path)?;
        let scan = format::scan(&mut log)?;
        if scan.tail_bytes > 0 {
            log.set_len(scan.valid_bytes)?;
            fault("recovery_truncate")?;
            log.sync_all()?;
            fault("recovery_sync")?;
        }
        // Also makes a preceding rename that survived a process crash durable before acknowledging new writes.
        sync_dir(&dir)?;
        log.seek(SeekFrom::End(0))?;
        Ok(Self {
            dir,
            _lock: guard,
            log,
            entries: scan.entries,
            records: scan.records,
            bytes: scan.valid_bytes,
            recovered_tail: scan.tail_bytes,
            poisoned: false,
        })
    }
    pub fn get(&self, key: &[u8]) -> Option<&[u8]> {
        self.entries.get(key).map(Vec::as_slice)
    }
    pub fn entries(&self) -> impl Iterator<Item = (&[u8], &[u8])> {
        self.entries
            .iter()
            .map(|(k, v)| (k.as_slice(), v.as_slice()))
    }
    pub fn stats(&self) -> Stats {
        Stats {
            live_keys: self.entries.len(),
            live_bytes: self
                .entries
                .iter()
                .map(|(k, v)| (k.len() + v.len()) as u64)
                .sum(),
            records: self.records,
            log_bytes: self.bytes,
            tail_bytes: self.recovered_tail,
        }
    }
    pub fn put(&mut self, key: &[u8], value: &[u8]) -> Result<()> {
        self.mutate(key, Some(value))
    }
    /// Appends a durable tombstone even when the key is absent.
    pub fn delete(&mut self, key: &[u8]) -> Result<()> {
        self.mutate(key, None)
    }
    fn mutate(&mut self, key: &[u8], value: Option<&[u8]>) -> Result<()> {
        if self.poisoned {
            return Err(Error::Poisoned);
        }
        let seq = self
            .records
            .checked_add(1)
            .ok_or(Error::InvalidInput("sequence exhausted; compact first"))?;
        let record = format::encode(seq, key, value)?;
        // From the first I/O until acknowledgement, any failure requires reopen.
        self.poisoned = true;
        self.log.write_all(&record[..format::RECORD_HEADER])?;
        fault("append_header")?;
        self.log.write_all(&record[format::RECORD_HEADER..])?;
        fault("append_payload")?;
        self.log.sync_all()?;
        fault("append_sync")?;
        match value {
            Some(v) => {
                self.entries.insert(key.to_vec(), v.to_vec());
            }
            None => {
                self.entries.remove(key);
            }
        }
        self.records = seq;
        self.bytes += record.len() as u64;
        self.poisoned = false;
        Ok(())
    }
    /// Rewrite current live keys atomically, keeping the separate lock inode stable.
    pub fn compact(&mut self) -> Result<()> {
        if self.poisoned {
            return Err(Error::Poisoned);
        }
        self.poisoned = true;
        let temp_path = self.dir.join("compact.tmp");
        let mut temp = File::create(&temp_path)?;
        temp.write_all(&format::file_header())?;
        fault("compact_header")?;
        let mut bytes = format::FILE_HEADER as u64;
        for (i, (k, v)) in self.entries.iter().enumerate() {
            let record = format::encode(i as u64 + 1, k, Some(v))?;
            temp.write_all(&record)?;
            bytes += record.len() as u64;
            fault("compact_record")?;
        }
        temp.sync_all()?;
        fault("compact_sync")?;
        fs::rename(&temp_path, self.dir.join("data.wal"))?;
        fault("compact_rename")?;
        sync_dir(&self.dir)?;
        fault("compact_dirsync")?;
        self.log = temp;
        self.records = self.entries.len() as u64;
        self.bytes = bytes;
        self.poisoned = false;
        Ok(())
    }
}
/// Inspect a pre-existing store without truncating a tail or creating files.
pub fn inspect(dir: impl AsRef<Path>) -> Result<Stats> {
    let dir = dir.as_ref();
    let _guard = lock(dir, false)?;
    let mut file = File::open(dir.join("data.wal"))?;
    let scan = format::scan(&mut file)?;
    Ok(Stats {
        live_keys: scan.entries.len(),
        live_bytes: scan
            .entries
            .iter()
            .map(|(k, v)| (k.len() + v.len()) as u64)
            .sum(),
        records: scan.records,
        log_bytes: scan.valid_bytes + scan.tail_bytes,
        tail_bytes: scan.tail_bytes,
    })
}

#[cfg(doctest)]
#[doc = include_str!("../README.md")]
pub struct ReadmeExamples;
