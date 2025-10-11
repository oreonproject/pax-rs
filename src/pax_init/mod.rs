use crate::{Command, Flag, PostAction, StateBox};
use settings::SettingsYaml;
use settings::acquire_lock;
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
    match acquire_lock() {
        Ok(Some(PostAction::PullSources)) => (),
        Ok(Some(action)) => return action,
        Err(fault) => return PostAction::Fuck(fault),
        _ => (),
    }
    if states.get::<bool>("force").is_none_or(|x| !*x) {
        println!(
            "\x1B[33m===== WARNING! WARNING! WARNING! =====\x1B[0m
This command should \x1B[31mNOT\x1B[0m be run as part of a standard update procedure.
To continue anyway, run with flag `\x1B[35m--{LONG_NAME}\x1B[0m`."
        );
    } else {
        println!("Pulling sources...");
        let Ok(runtime) = Runtime::new() else {
            return PostAction::Fuck(String::from("Error creating runtime!"));
        };
        let result = runtime.block_on(get_sources());
        if let Err(fault) = write_sources(result) {
            return PostAction::Fuck(fault);
        } else {
            println!("Done!");
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
    let mut settings = SettingsYaml::get_settings()?;
    settings.sources = sources
        .unwrap_or_default()
        .trim()
        .split('\n')
        .map(|x| x.to_string())
        .collect();
    settings.set_settings()
}
