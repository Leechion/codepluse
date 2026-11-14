//! Safe read/write for the JSON stores under `.data/`.
//!
//! Why this exists: the Go side writes those files with `os.WriteFile`, which
//! truncates and rewrites in place. A parallel reader can observe a half-written
//! file, and two writers can silently drop each other's changes. That was
//! tolerable while Go was the only writer; it is not once Rust starts writing
//! the same files during the migration.
//!
//! Two properties are provided here and used by every ported store:
//!
//! 1. **Atomic replace** — write to a temp file in the same directory, fsync,
//!    then rename over the target. Readers see either the old file or the new
//!    one, never a truncated one.
//! 2. **Cross-process write lock** — a `<file>.lock` created with `create_new`,
//!    so concurrent Rust writers serialise. Go does not take this lock, so it
//!    only protects Rust-to-Rust; combined with atomic replace that is enough
//!    because Go re-reads before every write (see the porting rule in
//!    docs/RUST-BACKEND-MIGRATION.md: only re-read stores may be ported).
//!
//! Reads are tolerant: a parse failure is retried briefly so a file being
//! replaced by the other implementation does not surface as an error.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime};

use serde_json::Value;

const READ_ATTEMPTS: usize = 6;
const READ_BACKOFF: Duration = Duration::from_millis(8);
const STALE_LOCK_AFTER: Duration = Duration::from_secs(30);
const LOCK_TIMEOUT: Duration = Duration::from_secs(5);

/// Holds the write lock; releases it on drop.
pub struct WriteLock {
    path: PathBuf,
}

impl Drop for WriteLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn lock_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".lock");
    PathBuf::from(name)
}

/// Serialises writers of one file. Stale locks (crashed writer) are stolen
/// after `STALE_LOCK_AFTER`.
pub fn lock(path: &Path) -> Result<WriteLock, String> {
    // The lock file lives beside the target, so the directory must exist first
    // (a brand-new workspace has no `.data/` yet).
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("创建目录失败：{e}"))?;
    }
    let lock = lock_path(path);
    let deadline = Instant::now() + LOCK_TIMEOUT;
    loop {
        match OpenOptions::new().write(true).create_new(true).open(&lock) {
            Ok(mut handle) => {
                let _ = write!(handle, "{}", std::process::id());
                let _ = handle.sync_all();
                return Ok(WriteLock { path: lock });
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                if let Ok(meta) = fs::metadata(&lock) {
                    if let Ok(modified) = meta.modified() {
                        if SystemTime::now()
                            .duration_since(modified)
                            .map(|age| age > STALE_LOCK_AFTER)
                            .unwrap_or(false)
                        {
                            let _ = fs::remove_file(&lock);
                            continue;
                        }
                    }
                }
                if Instant::now() >= deadline {
                    return Err("等待文件锁超时".into());
                }
                sleep(Duration::from_millis(10));
            }
            Err(err) => return Err(format!("创建文件锁失败：{err}")),
        }
    }
}

/// Reads JSON, returning `None` when the file does not exist. Transient parse
/// failures are retried: the other implementation may be replacing the file.
pub fn read_json(path: &Path) -> Result<Option<Value>, String> {
    let mut last_error = String::new();
    for attempt in 0..READ_ATTEMPTS {
        match fs::read(path) {
            Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
                Ok(value) => return Ok(Some(value)),
                Err(err) => {
                    last_error = format!("解析 JSON 失败：{err}");
                    if attempt + 1 < READ_ATTEMPTS {
                        sleep(READ_BACKOFF);
                        continue;
                    }
                }
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => {
                last_error = format!("读取文件失败：{err}");
                if attempt + 1 < READ_ATTEMPTS {
                    sleep(READ_BACKOFF);
                    continue;
                }
            }
        }
    }
    Err(last_error)
}

/// Replaces the file atomically: temp file in the same directory, fsync, rename.
pub fn write_json(path: &Path, value: &Value) -> Result<(), String> {
    let body = serde_json::to_vec_pretty(value).map_err(|e| format!("序列化失败：{e}"))?;
    write_bytes(path, &body)
}

/// Atomic replace for pre-encoded bytes — `write_json` without a formatting
/// opinion, used by the store writer that must match Go's byte layout.
pub fn write_bytes(path: &Path, body: &[u8]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("创建目录失败：{e}"))?;
    }
    let mut tmp_name = path.as_os_str().to_os_string();
    tmp_name.push(format!(
        ".tmp-{}-{:08x}",
        std::process::id(),
        rand::random::<u32>()
    ));
    let tmp = PathBuf::from(tmp_name);

    {
        let mut handle = File::create(&tmp).map_err(|e| format!("创建临时文件失败：{e}"))?;
        handle
            .write_all(body)
            .map_err(|e| format!("写入临时文件失败：{e}"))?;
        let _ = handle.sync_all();
    }

    match fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = fs::remove_file(&tmp);
            Err(format!("替换文件失败：{err}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "codepluse-store-file-{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn missing_file_reads_as_none() {
        let dir = temp_dir("missing");
        assert!(read_json(&dir.join("nope.json")).unwrap().is_none());
    }

    #[test]
    fn write_then_read_roundtrips() {
        let dir = temp_dir("roundtrip");
        let path = dir.join("data.json");
        write_json(&path, &json!({"a": 1, "b": [1, 2, 3]})).unwrap();
        let value = read_json(&path).unwrap().unwrap();
        assert_eq!(value["b"][2], 3);
        // No temp files left behind.
        let leftovers: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn lock_is_exclusive_and_released() {
        let dir = temp_dir("lock");
        let path = dir.join("data.json");
        {
            let _guard = lock(&path).unwrap();
            // A second acquisition must time out quickly instead of nesting.
            let nested = {
                let deadline = Instant::now() + Duration::from_millis(120);
                let mut result = Ok(());
                loop {
                    match lock(&path) {
                        Ok(guard) => {
                            drop(guard);
                            result = Ok(());
                            break;
                        }
                        Err(err) => {
                            if Instant::now() >= deadline {
                                result = Err(err);
                                break;
                            }
                            sleep(Duration::from_millis(10));
                        }
                    }
                }
                result
            };
            assert!(nested.is_err(), "锁重入必须失败");
        }
        let again = lock(&path).unwrap();
        drop(again);
    }

    /// 8 writers × 25 increments must not lose a single update.
    #[test]
    fn concurrent_writers_do_not_lose_updates() {
        let dir = temp_dir("concurrent");
        let path = dir.join("counter.json");
        write_json(&path, &json!({"count": 0})).unwrap();

        let threads = 8;
        let per_thread = 25;
        let errors = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();

        for _ in 0..threads {
            let path = path.clone();
            let errors = errors.clone();
            handles.push(std::thread::spawn(move || {
                for _ in 0..per_thread {
                    // The invariant under test is "no lost update". Under a
                    // fully parallel `cargo test` run the 5s lock timeout can
                    // legitimately fire, so retry a few times instead of
                    // turning scheduler pressure into a false failure.
                    let mut guard = None;
                    for attempt in 0..5 {
                        match lock(&path) {
                            Ok(got) => {
                                guard = Some(got);
                                break;
                            }
                            Err(_) if attempt < 4 => {
                                sleep(Duration::from_millis(20));
                            }
                            Err(_) => break,
                        }
                    }
                    let guard = match guard {
                        Some(guard) => guard,
                        None => {
                            errors.fetch_add(1, Ordering::SeqCst);
                            continue;
                        }
                    };
                    let current = read_json(&path)
                        .ok()
                        .flatten()
                        .and_then(|v| v["count"].as_i64())
                        .unwrap_or(0);
                    if write_json(&path, &json!({"count": current + 1})).is_err() {
                        errors.fetch_add(1, Ordering::SeqCst);
                    }
                    drop(guard);
                }
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        assert_eq!(errors.load(Ordering::SeqCst), 0);
        let value = read_json(&path).unwrap().unwrap();
        assert_eq!(
            value["count"].as_i64().unwrap(),
            (threads * per_thread) as i64
        );
    }

    /// A reader hammering the file while it is replaced must always parse.
    #[test]
    fn readers_never_observe_partial_json() {
        let dir = temp_dir("partial");
        let path = dir.join("big.json");

        let payload = json!({"rows": (0..4000).map(|i| json!({"id": i, "value": "x".repeat(40)})).collect::<Vec<_>>()});
        write_json(&path, &payload).unwrap();

        let stop = Arc::new(AtomicUsize::new(0));
        let writer = {
            let path = path.clone();
            let payload = payload.clone();
            let stop = stop.clone();
            std::thread::spawn(move || {
                for _ in 0..60 {
                    write_json(&path, &payload).unwrap();
                }
                stop.store(1, Ordering::SeqCst);
            })
        };

        let mut reads = 0;
        while stop.load(Ordering::SeqCst) == 0 {
            let value = read_json(&path).expect("读取失败");
            assert!(value.is_some(), "文件不应消失");
            reads += 1;
        }
        writer.join().unwrap();
        assert!(reads > 0, "读取线程没有执行");
    }
}
