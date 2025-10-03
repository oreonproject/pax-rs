use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::time::SystemTime;

const LOG_DIR: &str = "/var/log/pax";
const LOG_FILE: &str = "pax.log";
const TRANSACTION_LOG: &str = "transactions.log";

// Log levels
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    Debug,
    Info,
    Warning,
    Error,
}

impl LogLevel {
    fn as_str(&self) -> &str {
        match self {
            LogLevel::Debug => "DEBUG",
            LogLevel::Info => "INFO",
            LogLevel::Warning => "WARN",
            LogLevel::Error => "ERROR",
        }
    }
}

// Simple logger for pax operations
pub struct Logger {
    log_path: PathBuf,
    transaction_log_path: PathBuf,
    min_level: LogLevel,
    debug_mode: bool,
}

impl Logger {
    // Create a new logger
    pub fn new() -> Result<Self, String> {
        Self::with_dir(LOG_DIR)
    }

    // Create logger with custom directory
    pub fn with_dir(log_dir: &str) -> Result<Self, String> {
        let log_dir = PathBuf::from(log_dir);
        
        // Create log directory if it doesn't exist
        fs::create_dir_all(&log_dir)
            .map_err(|e| format!("Failed to create log directory: {}", e))?;

        let log_path = log_dir.join(LOG_FILE);
        let transaction_log_path = log_dir.join(TRANSACTION_LOG);

        // Check if debug mode is enabled via env var
        let debug_mode = std::env::var("PAX_DEBUG").is_ok();
        let min_level = if debug_mode {
            LogLevel::Debug
        } else {
            LogLevel::Info
        };

        Ok(Logger {
            log_path,
            transaction_log_path,
            min_level,
            debug_mode,
        })
    }

    // Log a message at the specified level
    pub fn log(&self, level: LogLevel, message: &str) {
        if level < self.min_level {
            return;
        }

        let timestamp = Self::format_timestamp();
        let log_line = format!("[{}] [{}] {}\n", timestamp, level.as_str(), message);

        // Write to file
        if let Err(e) = self.write_to_file(&self.log_path, &log_line) {
            eprintln!("Failed to write to log file: {}", e);
        }

        // Also print to stderr for errors and warnings
        if level >= LogLevel::Warning {
            eprint!("{}", log_line);
        } else if self.debug_mode {
            eprint!("{}", log_line);
        }
    }

    // Convenience methods
    pub fn debug(&self, message: &str) {
        self.log(LogLevel::Debug, message);
    }

    pub fn info(&self, message: &str) {
        self.log(LogLevel::Info, message);
    }

    pub fn warning(&self, message: &str) {
        self.log(LogLevel::Warning, message);
    }

    pub fn error(&self, message: &str) {
        self.log(LogLevel::Error, message);
    }

    // Log a transaction for potential rollback
    pub fn log_transaction(&self, operation: &str, details: &str) -> Result<(), String> {
        let timestamp = Self::format_timestamp();
        let log_line = format!("[{}] {} | {}\n", timestamp, operation, details);

        self.write_to_file(&self.transaction_log_path, &log_line)
    }

    // Write to a log file
    fn write_to_file(&self, path: &PathBuf, content: &str) -> Result<(), String> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| format!("Failed to open log file: {}", e))?;

        file.write_all(content.as_bytes())
            .map_err(|e| format!("Failed to write to log: {}", e))?;

        Ok(())
    }

    // Format current timestamp
    fn format_timestamp() -> String {
        let now = SystemTime::now();
        match now.duration_since(SystemTime::UNIX_EPOCH) {
            Ok(duration) => {
                let secs = duration.as_secs();
                // Simple timestamp formatting
                chrono::DateTime::from_timestamp(secs as i64, 0)
                    .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
                    .unwrap_or_else(|| format!("{}", secs))
            }
            Err(_) => "UNKNOWN".to_string(),
        }
    }

    // Read transaction log for rollback purposes
    pub fn read_transaction_log(&self) -> Result<Vec<String>, String> {
        let content = fs::read_to_string(&self.transaction_log_path)
            .map_err(|e| format!("Failed to read transaction log: {}", e))?;

        Ok(content.lines().map(|s| s.to_string()).collect())
    }

    // Clear transaction log (after successful completion)
    pub fn clear_transaction_log(&self) -> Result<(), String> {
        fs::write(&self.transaction_log_path, "")
            .map_err(|e| format!("Failed to clear transaction log: {}", e))
    }

    // Rotate logs if they get too large
    pub fn rotate_if_needed(&self) -> Result<(), String> {
        const MAX_LOG_SIZE: u64 = 10 * 1024 * 1024; // 10 MB

        if let Ok(metadata) = fs::metadata(&self.log_path) {
            if metadata.len() > MAX_LOG_SIZE {
                let backup_path = self.log_path.with_extension("log.old");
                fs::rename(&self.log_path, backup_path)
                    .map_err(|e| format!("Failed to rotate log: {}", e))?;
            }
        }

        Ok(())
    }
}

impl Default for Logger {
    fn default() -> Self {
        Self::new().expect("Failed to create logger")
    }
}

// Global logger instance using OnceLock for thread-safe initialization
use std::sync::OnceLock;
static GLOBAL_LOGGER: OnceLock<Logger> = OnceLock::new();

// Initialize global logger
pub fn init_logger() -> Result<(), String> {
    GLOBAL_LOGGER.get_or_init(|| {
        Logger::new().unwrap_or_else(|_| {
            // Fallback logger if we can't create the normal one
            Logger {
                log_path: std::path::PathBuf::from("/tmp/pax.log"),
                transaction_log_path: std::path::PathBuf::from("/tmp/pax-transactions.log"),
                min_level: LogLevel::Info,
                debug_mode: false,
            }
        })
    });
    Ok(())
}

// Get global logger
pub fn get_logger() -> &'static Logger {
    GLOBAL_LOGGER.get().expect("Logger not initialized - call init_logger() first")
}

// Convenience macros for logging
#[macro_export]
macro_rules! log_debug {
    ($($arg:tt)*) => {
        $crate::logging::get_logger().debug(&format!($($arg)*))
    };
}

#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => {
        $crate::logging::get_logger().info(&format!($($arg)*))
    };
}

#[macro_export]
macro_rules! log_warning {
    ($($arg:tt)*) => {
        $crate::logging::get_logger().warning(&format!($($arg)*))
    };
}

#[macro_export]
macro_rules! log_error {
    ($($arg:tt)*) => {
        $crate::logging::get_logger().error(&format!($($arg)*))
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_logger_creation() {
        let temp_dir = TempDir::new().unwrap();
        let logger = Logger::with_dir(temp_dir.path().to_str().unwrap());
        assert!(logger.is_ok());
    }

    #[test]
    fn test_logging() {
        let temp_dir = TempDir::new().unwrap();
        let logger = Logger::with_dir(temp_dir.path().to_str().unwrap()).unwrap();
        
        logger.info("Test message");
        logger.error("Test error");
        
        let log_content = fs::read_to_string(temp_dir.path().join(LOG_FILE)).unwrap();
        assert!(log_content.contains("Test message"));
        assert!(log_content.contains("Test error"));
    }

    #[test]
    fn test_transaction_log() {
        let temp_dir = TempDir::new().unwrap();
        let logger = Logger::with_dir(temp_dir.path().to_str().unwrap()).unwrap();
        
        logger.log_transaction("INSTALL", "package-name").unwrap();
        
        let transactions = logger.read_transaction_log().unwrap();
        assert_eq!(transactions.len(), 1);
        assert!(transactions[0].contains("INSTALL"));
    }
}

