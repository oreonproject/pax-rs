use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const LOCK_FILE: &str = "/var/run/pax.lock";
const LOCK_TIMEOUT_SECS: u64 = 300; // 5 minutes - consider lock stale after this

// Process lock to prevent concurrent pax operations
pub struct ProcessLock {
    lock_path: PathBuf,
    acquired: bool,
}

impl ProcessLock {
    // Try to acquire the process lock
    pub fn acquire() -> Result<Self, String> {
        Self::acquire_with_path(LOCK_FILE)
    }

    // Acquire lock with custom path (for testing)
    pub fn acquire_with_path(path: &str) -> Result<Self, String> {
        let lock_path = PathBuf::from(path);

        // Check if lock file exists
        if lock_path.exists() {
            // Check if its stale
            if Self::is_stale(&lock_path)? {
                println!("Cleaning stale lock file...");
                fs::remove_file(&lock_path)
                    .map_err(|e| format!("Failed to remove stale lock: {}", e))?;
            } else {
                // Read the PID from lock file
                let pid = Self::read_lock_pid(&lock_path)?;
                return Err(format!(
                    "Another pax process is running (PID: {}). Please wait for it to complete.",
                    pid
                ));
            }
        }

        // Create lock file with current PID
        let pid = std::process::id();
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        
        let lock_content = format!("{}\n{}", pid, timestamp);

        // Ensure parent directory exists
        if let Some(parent) = lock_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create lock directory: {}", e))?;
        }

        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
            .map_err(|e| format!("Failed to create lock file: {}", e))?;

        file.write_all(lock_content.as_bytes())
            .map_err(|e| format!("Failed to write lock file: {}", e))?;

        Ok(ProcessLock {
            lock_path,
            acquired: true,
        })
    }

    // Check if lock file is stale
    fn is_stale(lock_path: &Path) -> Result<bool, String> {
        let mut file = File::open(lock_path)
            .map_err(|e| format!("Failed to open lock file: {}", e))?;
        
        let mut contents = String::new();
        file.read_to_string(&mut contents)
            .map_err(|e| format!("Failed to read lock file: {}", e))?;

        let lines: Vec<&str> = contents.lines().collect();
        if lines.len() < 2 {
            // malformed lock file, consider it stale
            return Ok(true);
        }

        let pid: u32 = lines[0].parse()
            .map_err(|_| "Invalid PID in lock file")?;
        let timestamp: u64 = lines[1].parse()
            .map_err(|_| "Invalid timestamp in lock file")?;

        // Check if process is still running
        if !Self::is_process_running(pid) {
            return Ok(true);
        }

        // Check if lock is too old
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        
        if now - timestamp > LOCK_TIMEOUT_SECS {
            return Ok(true);
        }

        Ok(false)
    }

    // Check if a process with given PID is running
    fn is_process_running(pid: u32) -> bool {
        #[cfg(unix)]
        {
            // On Unix, check /proc/PID
            let proc_path = format!("/proc/{}", pid);
            Path::new(&proc_path).exists()
        }

        #[cfg(not(unix))]
        {
            // fallback - assume its running to be safe
            true
        }
    }

    // Read PID from lock file
    fn read_lock_pid(lock_path: &Path) -> Result<u32, String> {
        let mut file = File::open(lock_path)
            .map_err(|e| format!("Failed to open lock file: {}", e))?;
        
        let mut contents = String::new();
        file.read_to_string(&mut contents)
            .map_err(|e| format!("Failed to read lock file: {}", e))?;

        let lines: Vec<&str> = contents.lines().collect();
        if lines.is_empty() {
            return Err("Empty lock file".to_string());
        }

        lines[0].parse()
            .map_err(|_| "Invalid PID in lock file".to_string())
    }

    // Release the lock (called automatically on drop, but can be called explicitly)
    pub fn release(&mut self) -> Result<(), String> {
        if self.acquired {
            fs::remove_file(&self.lock_path)
                .map_err(|e| format!("Failed to remove lock file: {}", e))?;
            self.acquired = false;
        }
        Ok(())
    }
}

impl Drop for ProcessLock {
    fn drop(&mut self) {
        if self.acquired {
            let _ = fs::remove_file(&self.lock_path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_acquire_lock() {
        let temp_dir = TempDir::new().unwrap();
        let lock_path = temp_dir.path().join("test.lock");
        let lock_path_str = lock_path.to_str().unwrap();

        let lock = ProcessLock::acquire_with_path(lock_path_str);
        assert!(lock.is_ok());
    }

    #[test]
    fn test_concurrent_lock_fails() {
        let temp_dir = TempDir::new().unwrap();
        let lock_path = temp_dir.path().join("test.lock");
        let lock_path_str = lock_path.to_str().unwrap();

        let _lock1 = ProcessLock::acquire_with_path(lock_path_str).unwrap();
        let lock2 = ProcessLock::acquire_with_path(lock_path_str);
        
        assert!(lock2.is_err());
    }

    #[test]
    fn test_lock_release() {
        let temp_dir = TempDir::new().unwrap();
        let lock_path = temp_dir.path().join("test.lock");
        let lock_path_str = lock_path.to_str().unwrap();

        {
            let _lock = ProcessLock::acquire_with_path(lock_path_str).unwrap();
            // lock should be held here
        }
        // lock should be released after drop

        let lock2 = ProcessLock::acquire_with_path(lock_path_str);
        assert!(lock2.is_ok());
    }
}

