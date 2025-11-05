use commands::Command;
use flags::Flag;
use settings::OriginKind;
use settings::SettingsYaml;
use settings::acquire_lock;
use statebox::StateBox;
use tokio::runtime::Runtime;
use utils::PostAction;
use utils::err;

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
        if let Err(fault) = runtime.block_on(gen_sources()) {
            return PostAction::Fuck(fault);
        } else {
            println!("Done!");
        }
    }
    PostAction::Return
}

async fn gen_sources() -> Result<(), String> {
    let Some(sources) = reqwest::get(
        "https://raw.githubusercontent.com/oreonproject/pax-rs/refs/heads/main/endpoints.txt",
    )
    .await
    .ok() else {
        return err!("Failed to locate sources!");
    };
    let Some(sources) = sources.text().await.ok() else {
        return err!("Failed to read pulled sources!");
    };
    let mut settings = SettingsYaml::get_settings()?;
    for source in sources.trim().split('\n') {
        // make this actually detect the source type
        let source = OriginKind::Pax(source.to_string());
        settings.sources.push(source);
    }
    settings.set_settings()
}
