use crate::crypto;
use crate::{Command, PostAction, StateBox};
use nix::unistd;
use std::path::Path;

pub fn build(hierarchy: &[String]) -> Command {
    Command::new(
        "trust",
        vec![],
        "Manage trusted repository keys",
        Vec::new(),
        Some(vec![add, remove, list]),
        |_, _| PostAction::GetHelp,
        hierarchy,
    )
}

// Add subcommand
pub fn add(hierarchy: &[String]) -> Command {
    Command::new(
        "add",
        vec![],
        "Add a trusted repository key",
        Vec::new(),
        None,
        run_add,
        hierarchy,
    )
}

fn run_add(_: &StateBox, args: Option<&[String]>) -> PostAction {
    // check for root
    let euid = unistd::geteuid();
    if euid.as_raw() != 0 {
        return PostAction::Elevate;
    }

    let args = match args {
        None => {
            println!("Usage: pax trust add <key-file> [key-name]");
            return PostAction::Return;
        }
        Some(args) => args,
    };

    if args.is_empty() {
        println!("Usage: pax trust add <key-file> [key-name]");
        return PostAction::Return;
    }

    let key_file = &args[0];
    let key_name = if args.len() > 1 {
        args[1].clone()
    } else {
        // derive name from filename
        Path::new(key_file)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("repo_key")
            .to_string()
    };

    // load the key
    let key_bytes = match crypto::load_public_key(key_file) {
        Ok(bytes) => bytes,
        Err(e) => {
            println!("Failed to load key: {}", e);
            return PostAction::Return;
        }
    };

    // add to trust store
    match crypto::add_trusted_key(&key_name, &key_bytes) {
        Ok(()) => {
            println!("Added trusted key: {}", key_name);
            println!("Key fingerprint: {}", hex::encode(&key_bytes[..8]));
        }
        Err(e) => {
            println!("Failed to add key: {}", e);
        }
    }

    PostAction::Return
}

// Remove subcommand
pub fn remove(hierarchy: &[String]) -> Command {
    Command::new(
        "remove",
        vec![String::from("rm")],
        "Remove a trusted repository key",
        Vec::new(),
        None,
        run_remove,
        hierarchy,
    )
}

fn run_remove(_: &StateBox, args: Option<&[String]>) -> PostAction {
    // check for root
    let euid = unistd::geteuid();
    if euid.as_raw() != 0 {
        return PostAction::Elevate;
    }

    let args = match args {
        None => {
            println!("Usage: pax trust remove <key-name>");
            return PostAction::Return;
        }
        Some(args) => args,
    };

    if args.is_empty() {
        println!("Usage: pax trust remove <key-name>");
        return PostAction::Return;
    }

    let key_name = &args[0];

    // confirm removal
    print!("Remove trusted key '{}'? [y/N]: ", key_name);
    let _ = std::io::Write::flush(&mut std::io::stdout());
    let mut input = String::new();
    if std::io::stdin().read_line(&mut input).is_err() {
        println!("Failed to read input");
        return PostAction::Return;
    }
    
    if !["yes", "y"].contains(&input.trim().to_lowercase().as_str()) {
        println!("Cancelled");
        return PostAction::Return;
    }

    // remove key
    match crypto::remove_trusted_key(key_name) {
        Ok(()) => {
            println!("Removed trusted key: {}", key_name);
        }
        Err(e) => {
            println!("Failed to remove key: {}", e);
        }
    }

    PostAction::Return
}

// List subcommand
pub fn list(hierarchy: &[String]) -> Command {
    Command::new(
        "list",
        vec![String::from("ls")],
        "List all trusted repository keys",
        Vec::new(),
        None,
        run_list,
        hierarchy,
    )
}

fn run_list(_: &StateBox, _args: Option<&[String]>) -> PostAction {
    match crypto::list_trusted_keys() {
        Ok(keys) => {
            if keys.is_empty() {
                println!("No trusted keys configured");
                println!("\nAdd repository keys with: pax trust add <key-file>");
                return PostAction::Return;
            }

            println!("Trusted repository keys:\n");
            
            for key_name in keys {
                // try to load key to show fingerprint
                let key_path = format!("/etc/pax/trusted-keys/{}.pub", key_name);
                
                if let Ok(key_bytes) = crypto::load_public_key(&key_path) {
                    let fingerprint = hex::encode(&key_bytes[..8]);
                    println!("  \x1B[33m{}\x1B[0m", key_name);
                    println!("    Fingerprint: {}", fingerprint);
                } else {
                    println!("  \x1B[33m{}\x1B[0m", key_name);
                    println!("    (failed to load key)");
                }
            }
        }
        Err(e) => {
            println!("Failed to list keys: {}", e);
        }
    }

    PostAction::Return
}

