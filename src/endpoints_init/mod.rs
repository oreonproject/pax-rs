use crate::{Command, Flag, PostAction, StateBox};
use nix::unistd;
use settings::{get_settings, set_settings};
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
        let runtime = match Runtime::new() {
            Ok(runtime) => runtime,
            Err(_) => {
                println!("Error creating runtime!");
                return PostAction::Return;
            }
        };
        let result = runtime.block_on(get_sources());
        match write_sources(result) {
            Ok(()) => {
                println!("Done!");
            }
            Err(e) => {
                println!("Failed to save sources! Are you sudo?");
                println!("Reported error: `{e}`");
                return PostAction::Return;
            }
        }
    }
    PostAction::Return
}

async fn get_sources() -> Option<String> {
    reqwest::get(
        "https://raw.githubusercontent.com/oreonproject/pax-rs/refs/heads/main/endpoints.txt",
    )
    .await
    .ok()?
    .text()
    .await
    .ok()
}

fn write_sources(sources: Option<String>) -> Result<(), String> {
    let mut settings = get_settings()?;
    settings.sources = sources
        .unwrap_or_default()
        .trim()
        .split('\n')
        .map(|x| x.to_string())
        .collect();
    set_settings(settings)
}
