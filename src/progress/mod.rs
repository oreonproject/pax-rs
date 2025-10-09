use indicatif::{ProgressBar, ProgressStyle};
use std::time::Duration;

pub struct ProgressIndicator {
    bar: ProgressBar,
}

impl ProgressIndicator {
    // Create a new progress bar with total size
    pub fn new(total: u64, message: &str) -> Self {
        let bar = ProgressBar::new(total);
        bar.set_style(
            ProgressStyle::default_bar()
                .template(&format!("{} [{{bar:40.cyan/blue}}] {{percent}}% ({{eta}}) {{msg}}", message))
                .unwrap()
                .progress_chars("=>-")
        );
        ProgressIndicator { bar }
    }

    // Create a spinner for indeterminate progress
    pub fn new_spinner(message: &str) -> Self {
        let bar = ProgressBar::new_spinner();
        bar.set_style(
            ProgressStyle::default_spinner()
                .template(&format!("{} {{spinner}} {{msg}}", message))
                .unwrap()
                .tick_chars("⠁⠂⠄⡀⢀⠠⠐⠈")
        );
        bar.enable_steady_tick(Duration::from_millis(100));
        ProgressIndicator { bar }
    }

    // Create a simple counter progress bar
    pub fn new_counter(total: u64, message: &str) -> Self {
        let bar = ProgressBar::new(total);
        bar.set_style(
            ProgressStyle::default_bar()
                .template(&format!("{} [{{bar:40.green/blue}}] {{pos}}/{{len}}", message))
                .unwrap()
                .progress_chars("##-")
        );
        ProgressIndicator { bar }
    }

    // Update progress
    pub fn inc(&self, delta: u64) {
        self.bar.inc(delta);
    }

    // Set progress position
    pub fn set_position(&self, pos: u64) {
        self.bar.set_position(pos);
    }

    // Set message
    pub fn set_message(&self, msg: &str) {
        self.bar.set_message(msg.to_string());
    }

    // Finish with message
    pub fn finish(&self, msg: &str) {
        self.bar.finish_with_message(msg.to_string());
    }

    // Finish and clear
    pub fn finish_and_clear(&self) {
        self.bar.finish_and_clear();
    }
}

// Simple text-based progress bar for operations without size information
pub fn create_simple_progress(message: &str) -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::default_spinner()
            .template(&format!("{} {{spinner}}", message))
            .unwrap()
            .tick_chars("/-\\|")
    );
    pb.enable_steady_tick(Duration::from_millis(80));
    pb
}

// Create a multi-progress bar for parallel operations
pub fn create_progress_bar(total: u64, prefix: &str) -> ProgressBar {
    let pb = ProgressBar::new(total);
    pb.set_style(
        ProgressStyle::default_bar()
            .template(&format!("{} [{{bar:40.cyan/blue}}] {{pos}}/{{len}} ({{percent}}%)", prefix))
            .unwrap()
            .progress_chars("=>-")
    );
    pb
}

