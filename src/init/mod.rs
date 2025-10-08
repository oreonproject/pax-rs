use std::fs;

use crate::{Command, PostAction, StateBox};

const SYSTEM_SETTINGS_PATH: &str = "/etc/pax/settings.yaml";
const USER_SETTINGS_PATH: &str = "/tmp/pax/settings.yaml";
const URL: &str = "http://localhost:8080";

pub fn build(hierarchy: &[String]) -> Command {
    Command::new(
        "init",
        vec![String::from("i")],
        "Initialize pax",
        Vec::new(),
        None,
        run,
        hierarchy,
    )
}

fn run(_: &StateBox, args: Option<&[String]>) -> PostAction {
    if let Some(a) = args {
        if !a.is_empty() {
            println!("Usage: pax init");
            return PostAction::Return;
        }
    }

    println!("Initializing pax...");

    // Try system paths first, fall back to user paths for development
    let settings_path = if fs::create_dir_all("/etc/pax").is_ok() {
        // System paths are writable, use system location
        SYSTEM_SETTINGS_PATH
    } else {
        // Fall back to user paths for development
        println!("Note: Using user-local paths for development. For system-wide installation, run with appropriate permissions.");
        let _ = fs::create_dir_all("/tmp/pax");
        USER_SETTINGS_PATH
    };

    // Create settings.yaml with default configuration
    let settings_content = format!(
        "sources:\n  - {}\ndb_path: /opt/pax/db/pax.db\nstore_path: /opt/pax/store\ncache_path: /var/cache/pax\nlinks_path: /opt/pax/links\nparallel_downloads: 3\nverify_signatures: true\n",
        URL
    );

    match fs::write(settings_path, settings_content) {
        Ok(_) => (),
        Err(e) => {
            eprintln!("Failed to create {}: {}", settings_path, e);
            return PostAction::Return;
        }
    }

    println!("Pax initialized successfully.");
    PostAction::Return
}