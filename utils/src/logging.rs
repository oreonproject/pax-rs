use std::{
    fs::OpenOptions,
    io::Write,
    path::PathBuf,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

impl std::fmt::Display for LogLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LogLevel::Debug => write!(f, "DEBUG"),
            LogLevel::Info => write!(f, "INFO"),
            LogLevel::Warn => write!(f, "WARN"),
            LogLevel::Error => write!(f, "ERROR"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub timestamp: u64,
    pub level: LogLevel,
    pub module: String,
    pub message: String,
    pub details: Option<String>,
}

pub struct Logger {
    log_file: Option<PathBuf>,
    min_level: LogLevel,
    console_output: bool,
}

impl Logger {
    pub fn new() -> Self {
        Self {
            log_file: None,
            min_level: LogLevel::Info,
            console_output: true,
        }
    }
    
    pub fn with_file(mut self, path: PathBuf) -> Self {
        self.log_file = Some(path);
        self
    }
    
    pub fn with_min_level(mut self, level: LogLevel) -> Self {
        self.min_level = level;
        self
    }
    
    pub fn with_console_output(mut self, enabled: bool) -> Self {
        self.console_output = enabled;
        self
    }
    
    pub fn log(&self, level: LogLevel, module: &str, message: &str, details: Option<&str>) {
        // Check if we should log this level
        if !self.should_log(&level) {
            return;
        }
        
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        
        let entry = LogEntry {
            timestamp,
            level: level.clone(),
            module: module.to_string(),
            message: message.to_string(),
            details: details.map(|s| s.to_string()),
        };
        
        // Console output
        if self.console_output {
            let color = match level {
                LogLevel::Debug => "\x1B[90m", // Gray
                LogLevel::Info => "\x1B[94m",  // Blue
                LogLevel::Warn => "\x1B[93m",  // Yellow
                LogLevel::Error => "\x1B[91m", // Red
            };
            
            let reset = "\x1B[0m";
            println!("{}{} [{}] {}: {}{}", 
                color, 
                level, 
                module, 
                message, 
                details.map(|d| format!(" ({})", d)).unwrap_or_default(),
                reset
            );
        }
        
        // File output
        if let Some(ref log_file) = self.log_file {
            if let Ok(mut file) = OpenOptions::new()
                .create(true)
                .append(true)
                .open(log_file)
            {
                let log_line = serde_json::to_string(&entry).unwrap_or_default();
                let _ = writeln!(file, "{}", log_line);
            }
        }
    }
    
    fn should_log(&self, level: &LogLevel) -> bool {
        match (&self.min_level, level) {
            (LogLevel::Debug, _) => true,
            (LogLevel::Info, LogLevel::Info | LogLevel::Warn | LogLevel::Error) => true,
            (LogLevel::Warn, LogLevel::Warn | LogLevel::Error) => true,
            (LogLevel::Error, LogLevel::Error) => true,
            _ => false,
        }
    }
}

impl Default for Logger {
    fn default() -> Self {
        Self::new()
    }
}

// Global logger instance
static LOGGER: Mutex<Option<Logger>> = Mutex::new(None);

pub fn init_logger(log_file: Option<PathBuf>, min_level: LogLevel, console_output: bool) {
    let logger = Logger::new()
        .with_file(log_file.unwrap_or_else(|| PathBuf::from("/var/log/pax.log")))
        .with_min_level(min_level)
        .with_console_output(console_output);
    
    if let Ok(mut global_logger) = LOGGER.lock() {
        *global_logger = Some(logger);
    }
}

pub fn log_debug(module: &str, message: &str, details: Option<&str>) {
    if let Ok(logger) = LOGGER.lock() {
        if let Some(ref logger) = *logger {
            logger.log(LogLevel::Debug, module, message, details);
        }
    }
}

pub fn log_info(module: &str, message: &str, details: Option<&str>) {
    if let Ok(logger) = LOGGER.lock() {
        if let Some(ref logger) = *logger {
            logger.log(LogLevel::Info, module, message, details);
        }
    }
}

pub fn log_warn(module: &str, message: &str, details: Option<&str>) {
    if let Ok(logger) = LOGGER.lock() {
        if let Some(ref logger) = *logger {
            logger.log(LogLevel::Warn, module, message, details);
        }
    }
}

pub fn log_error(module: &str, message: &str, details: Option<&str>) {
    if let Ok(logger) = LOGGER.lock() {
        if let Some(ref logger) = *logger {
            logger.log(LogLevel::Error, module, message, details);
        }
    }
}

// Enhanced error handling macros
#[macro_export]
macro_rules! log_and_err {
    ($level:ident, $module:expr, $msg:expr, $details:expr) => {
        {
            $crate::logging::log_$level($module, $msg, Some($details));
            Err(format!("{}: {}", $msg, $details))
        }
    };
    ($level:ident, $module:expr, $msg:expr) => {
        {
            $crate::logging::log_$level($module, $msg, None);
            Err($msg.to_string())
        }
    };
}

#[macro_export]
macro_rules! log_and_return {
    ($level:ident, $module:expr, $msg:expr, $details:expr) => {
        {
            $crate::logging::log_$level($module, $msg, Some($details));
            return;
        }
    };
    ($level:ident, $module:expr, $msg:expr) => {
        {
            $crate::logging::log_$level($module, $msg, None);
            return;
        }
    };
}
