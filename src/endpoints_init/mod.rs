use std::{
    fs::{DirBuilder, File},
    io::Write,
    path::PathBuf,
};

use crate::{Command, Flag, StateBox, command::PostAction};
use nix::unistd;
use tokio::runtime::Runtime;

static LONG_NAME: &str = "force";

pub fn build(hierarchy: &[String]) -> Command {
    let force = Flag::new(
        None,
        LONG_NAME,
        "bypasses the warning before running the command",
        false,
        false,
        |states, _args| {
            states.shove("force", true);
        },
    );
    Command::new(
        "pax-init",
        Vec::new(),
        "Initializes the endpoints for pax",
        vec![force],
        None,
        get_endpoints,
        hierarchy,
    )
}

fn get_endpoints(states: &StateBox, _args: Option<&[String]>) -> PostAction {
    let euid = unistd::geteuid();
    if euid.as_raw() != 0 {
        return PostAction::Elevate;
    }
    if states.get::<bool>("force").is_none_or(|x| !*x) {
        println!(
            "\x1B[33m===== WARNING! WARNING! WARNING! =====\x1B[0m
This command should \x1B[31mNOT\x1B[0m be run as part of a standard update procedure.
To continue anyway, run with flag `\x1B[35m--{LONG_NAME}\x1B[0m`."
        );
    } else {
        println!("Pulling sources...");
        let _runtime = match Runtime::new() {
            Ok(runtime) => runtime,
            Err(_) => {
                println!("Error creating runtime!");
                return PostAction::Return;
            }
        };
        // result web request is commented out to prevent being ratelimited by GitHub during testing.
        // let result = runtime.block_on(get_sources());
        let result = Some(String::from("http://pax.local:8080\n"));
        if write_sources(result).is_none() {
            println!("Failed to save sources! Are you sudo?");
            return PostAction::Return;
        }
        println!("Done!");
    }
    PostAction::Return
}

async fn _get_sources() -> Option<String> {
    reqwest::get(
        "https://raw.githubusercontent.com/oreonproject/pax-rs/refs/heads/main/endpoints.txt",
    )
    .await
    .ok()?
    .text()
    .await
    .ok()
}

fn write_sources(sources: Option<String>) -> Option<usize> {
    let mut path = PathBuf::from("/etc/pax.d");
    if !path.exists() {
        DirBuilder::new().create(&path).ok()?;
    }
    path.push("sources.txt");
    if path.is_file() || !path.exists() {
        let mut file = File::create(path).ok()?;
        let bytes = file.write(sources?.as_bytes()).ok()?;
        Some(bytes)
    } else {
        None
    }
}
