use std::fs;
use std::fs::File;
use std::io::Write;
use std::path::Path;

use crate::{Command, PostAction, StateBox};

const PATH: &str = "/etc/pax/endpoints.txt";
const URL: &str = "http:localhost:8000";

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

    // create /etc/pax if it doesn't exist
    match fs::create_dir_all("/etc/pax"){
        Ok(_) => (),
        Err(e) => {
            println!("Failed to create /etc/pax directory: {}", e);
            return PostAction::Return;
        }
    }
    
    // create /etc/pax/endpoints.txt if it doesn't exist
    let file: File = match fs::File::create(PATH) {
        Ok(f) => f,
        Err(e) => {
            println!("Failed to create {}: {}", PATH, e);
            return PostAction::Return;
        }
    };

    // write default URL to endpoints.txt
    let mut file = file; 
    match file.write_all(URL.as_bytes()) {
        Ok(()) => (),
        Err(e) => {
            eprintln!("Failed to write {}: {}", Path::new(PATH).display(), e);
            return PostAction::Return;
        }
    }

    match file.flush() {
        Ok(()) => (),
        Err(e) => {
            eprintln!("Failed to flush {}: {}", Path::new(PATH).display(), e);
            return PostAction::Return;
        }
    }

    println!("Pax initialized successfully.");
    PostAction::Return
}