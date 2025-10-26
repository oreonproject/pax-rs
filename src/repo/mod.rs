use commands::Command;
use flags::Flag;
use settings::{OriginKind, SettingsYaml};
use statebox::StateBox;
use utils::{PostAction};

pub fn build(hierarchy: &[String]) -> Command {
    let list = Flag::new(
        Some('l'),
        "list",
        "List all configured repositories",
        false,
        false,
        |states, _| {
            states.shove("list_repos", true);
        },
    );
    
    let test = Flag::new(
        Some('t'),
        "test",
        "Test repository connectivity",
        true,
        false,
        |states, arg| {
            if let Some(repo_url) = arg {
                states.shove("test_repo", repo_url.clone());
            }
        },
    );

    Command::new(
        "repo",
        vec![String::from("repositories")],
        "Manage package repositories",
        vec![list, test],
        None,
        run,
        hierarchy,
    )
}

fn run(states: &StateBox, _args: Option<&[String]>) -> PostAction {
    let settings = match SettingsYaml::get_settings() {
        Ok(settings) => settings,
        Err(fault) => return PostAction::Fuck(fault),
    };

    if states.get::<bool>("list_repos").is_some_and(|x| *x) {
        return list_repositories(&settings);
    }
    
    if let Some(repo_url) = states.get::<String>("test_repo") {
        return test_repository(repo_url);
    }

    // Default to listing repositories if no specific action requested
    list_repositories(&settings)
}

fn list_repositories(settings: &SettingsYaml) -> PostAction {
    if settings.sources.is_empty() && settings.mirror_list.is_none() {
        println!("\x1B[95mNo repositories configured\x1B[0m");
        println!("\x1B[90mPopulate /etc/pax/sources.conf to configure repositories/mirrors.\x1B[0m");
        return PostAction::Return;
    }

    println!("\x1B[92mConfigured Repositories:\x1B[0m");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!();

    if let Some(list) = &settings.mirror_list {
        println!("\x1B[94mMirror List\x1B[0m");
        println!("   \x1B[90mURL:\x1B[0m {}", list);
        println!();
    }

    for (i, source) in settings.sources.iter().enumerate() {
        let (repo_type, url) = match source {
            OriginKind::Pax(url) => ("PAX", url.clone()),
            OriginKind::Github { user, repo } => ("GitHub", format!("https://github.com/{}/{}", user, repo)),
            OriginKind::Apt(url) => ("APT", format!("apt://{}", url)),
            OriginKind::Rpm(url) => ("RPM", format!("rpm://{}", url)),
            OriginKind::CloudflareR2 { bucket, account_id, .. } => {
                ("Cloudflare R2", format!("r2://{}.{}", bucket, account_id))
            },
            OriginKind::Deb(url) => ("DEB", format!("deb://{}", url)),
            OriginKind::Yum(url) => ("YUM", format!("yum://{}", url)),
        };

        println!("\x1B[94m{}. {}\x1B[0m", i + 1, repo_type);
        println!("   \x1B[90mURL:\x1B[0m {}", url);
        println!();
    }

    println!("\x1B[90mTotal: {} repository(ies)\x1B[0m", settings.sources.len());
    PostAction::Return
}

fn test_repository(repo_url: &str) -> PostAction {
    println!("Testing repository connectivity: {}", repo_url);
    
    // Simple connectivity test
    let test_url = if repo_url.starts_with("https://github.com/") {
        format!("{}/releases", repo_url)
    } else if repo_url.starts_with("apt://") {
        let apt_url = repo_url.strip_prefix("apt://").unwrap();
        format!("{}/Packages", apt_url)
    } else if repo_url.starts_with("rpm://") {
        let rpm_url = repo_url.strip_prefix("rpm://").unwrap();
        format!("{}/repodata/repomd.xml", rpm_url)
    } else {
        format!("{}/health", repo_url)
    };

    match reqwest::blocking::get(&test_url) {
        Ok(response) => {
            if response.status().is_success() {
                println!("\x1B[92mRepository is accessible\x1B[0m");
            } else {
                println!("\x1B[93mRepository responded with status: {}\x1B[0m", response.status());
            }
        }
        Err(e) => {
            println!("\x1B[91mFailed to connect to repository: {}\x1B[0m", e);
        }
    }

    PostAction::Return
}
