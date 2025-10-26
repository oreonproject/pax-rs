use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    thread::sleep,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use utils::{PostAction, err, get_dir, is_root};

#[derive(PartialEq, Serialize, Deserialize, Debug, Clone)]
pub struct SettingsYaml {
    pub locked: bool,
    pub version: String,
    pub arch: Arch,
    pub exec: Option<String>,
    #[serde(default)]
    pub mirror_list: Option<String>,
    pub sources: Vec<OriginKind>,
}

impl SettingsYaml {
    pub fn new() -> Self {
        let mut command = std::process::Command::new("/usr/bin/uname");
        let arch = if let Ok(output) = command.arg("-m").output() {
            match String::from_utf8_lossy(&output.stdout)
                .to_string()
                .as_str()
                .trim()
            {
                "x86_64" => {
                    let mut command = std::process::Command::new("/usr/bin/bash");
                    command.arg("-c").arg("(lscpu|grep -q avx512f&&echo 4&&exit||lscpu|grep -q avx2&&echo 3&&exit||lscpu|grep -q sse4_2&&echo 2&&exit||echo 1)");
                    if let Ok(output) = command.output() {
                        match String::from_utf8_lossy(&output.stdout)
                            .to_string()
                            .as_str()
                            .trim()
                        {
                            "4" | "3" => Arch::X86_64v3,
                            "2" | "1" => Arch::X86_64v1,
                            _ => Arch::NoArch,
                        }
                    } else {
                        Arch::NoArch
                    }
                }
                "aarch64" => Arch::Aarch64,
                "armv7l" => Arch::Armv7l,
                "armv8l" => Arch::Armv8l,
                _ => Arch::NoArch,
            }
        } else {
            Arch::NoArch
        };
        Self {
            locked: false,
            version: env!("SETTINGS_YAML_VERSION").to_string(),
            arch,
            exec: None,
            mirror_list: None,
            sources: Vec::new(),
        }
    }
    pub fn set_settings(self) -> Result<(), String> {
        let mut file = match File::create(affirm_path()?) {
            Ok(file) => file,
            Err(_) => return err!("Failed to open SettingsYaml as WO!"),
        };
        let settings = match serde_norway::to_string(&self) {
            Ok(settings) => settings,
            Err(_) => return err!("Failed to parse SettingsYaml to string!"),
        };
        match file.write_all(settings.as_bytes()) {
            Ok(_) => Ok(()),
            Err(_) => err!("Failed to write to file!"),
        }
    }
    pub fn get_settings() -> Result<Self, String> {
        let mut file = match File::open(affirm_path()?) {
            Ok(file) => file,
            Err(_) => return err!("Failed to open SettingsYaml as RO!"),
        };
        let mut data = String::new();
        if file.read_to_string(&mut data).is_err() {
            return err!("Failed to read file!");
        };
        let mut settings: SettingsYaml = match serde_norway::from_str(&data) {
            Ok(settings_yaml) => settings_yaml,
            Err(_) => {
                // If parsing fails, try to migrate from old format or create new settings
                println!("\x1B[93m[WARN] Settings file format is outdated or corrupted. Migrating to new format...\x1B[0m");
                let new_settings = Self::new();
                if let Err(e) = new_settings.clone().set_settings() {
                    return err!("Failed to create new settings file: {}", e);
                }
                new_settings
            }
        };
        let dir = get_dir()?;
        match load_sources_conf(&dir) {
            Ok((mirror, file_sources)) => {
                if mirror.is_some() {
                    settings.mirror_list = mirror;
                }
                if !file_sources.is_empty() {
                    settings.sources = file_sources;
                }
            }
            Err(fault) => {
                println!(
                    "\x1B[93m[WARN] Unable to load sources config: {}\x1B[0m",
                    fault
                );
            }
        }
        Ok(settings)
    }
}

#[derive(PartialEq, Eq, Deserialize, Serialize, Debug, Hash, Clone)]
pub enum OriginKind {
    Apt(String),
    Pax(String),
    Github { user: String, repo: String },
    Rpm(String),
    CloudflareR2 { 
        bucket: String, 
        account_id: String,
        access_key_id: Option<String>,
        secret_access_key: Option<String>,
        region: Option<String>,
    },
    Deb(String),  // Enhanced dpkg/deb support
    Yum(String), // Enhanced dnf/yum support
}

impl std::fmt::Display for OriginKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OriginKind::Pax(url) => write!(f, "PAX: {}", url),
            OriginKind::Github { user, repo } => write!(f, "GitHub: {}/{}", user, repo),
            OriginKind::Apt(url) => write!(f, "APT: {}", url),
            OriginKind::Rpm(url) => write!(f, "RPM: {}", url),
            OriginKind::CloudflareR2 { bucket, account_id, .. } => {
                write!(f, "Cloudflare R2: {}.{}", bucket, account_id)
            },
            OriginKind::Deb(url) => write!(f, "DEB: {}", url),
            OriginKind::Yum(url) => write!(f, "YUM: {}", url),
        }
    }
}

#[derive(PartialEq, Serialize, Deserialize, Debug, Clone)]
pub enum Arch {
    NoArch,
    X86_64v1,
    X86_64v3,
    Aarch64,
    Armv7l,
    Armv8l,
}

impl Default for SettingsYaml {
    fn default() -> Self {
        Self::new()
    }
}

fn load_sources_conf(dir: &Path) -> Result<(Option<String>, Vec<OriginKind>), String> {
    let path = dir.join("sources.conf");
    if !path.exists() {
        return Ok((None, Vec::new()));
    }
    let contents =
        fs::read_to_string(&path).map_err(|_| format!("Failed to read {}.", path.display()))?;
    let mut mirror = None;
    let mut sources = Vec::new();
    for (idx, line) in contents.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let mut entries = Vec::new();
        for part in trimmed.split_whitespace() {
            if let Some((key, value)) = part.split_once('=') {
                let key = key.trim().to_lowercase();
                let value = value
                    .trim_matches(|c| matches!(c, '"' | '\''))
                    .to_string();
                entries.push((key, value));
            }
        }

        let find = |needle: &str| -> Option<&str> {
            entries
                .iter()
                .find(|(key, _)| key == needle)
                .map(|(_, value)| value.as_str())
        };

        let source_type = find("sourcetype")
            .or_else(|| find("type"))
            .map(|s| s.to_lowercase());
        let source_url = find("url").map(|s| s.to_string());
        let provider = find("provider").map(|s| s.to_lowercase());

        match source_type.as_deref() {
            Some("mirror") => {
                if let Some(url) = source_url {
                    if mirror.is_none() {
                        mirror = Some(url);
                    }
                } else {
                    println!(
                        "\x1B[93m[WARN] Mirror entry missing url= on line {} of {}.\x1B[0m",
                        idx + 1,
                        path.display()
                    );
                }
            }
            Some("repo") | Some("repository") => {
                if let Some(url) = source_url {
                    if url.starts_with("http://") || url.starts_with("https://") {
                        let origin = match provider.as_deref() {
                            Some("apt") | Some("deb") => OriginKind::Apt(url.clone()),
                            Some("rpm") | Some("yum") | Some("dnf") => OriginKind::Rpm(url.clone()),
                            Some("dpkg") => OriginKind::Deb(url.clone()),
                            Some("cloudflare") | Some("r2") => {
                                // Parse Cloudflare R2 configuration
                                let bucket = find("bucket").unwrap_or("").to_string();
                                let account_id = find("account_id").unwrap_or("").to_string();
                                let access_key_id = find("access_key_id").map(|s| s.to_string());
                                let secret_access_key = find("secret_access_key").map(|s| s.to_string());
                                let region = find("region").map(|s| s.to_string());
                                
                                if bucket.is_empty() || account_id.is_empty() {
                                    println!(
                                        "\x1B[93m[WARN] Cloudflare R2 repository missing required bucket or account_id on line {} of {}.\x1B[0m",
                                        idx + 1,
                                        path.display()
                                    );
                                    continue;
                                }
                                
                                OriginKind::CloudflareR2 {
                                    bucket,
                                    account_id,
                                    access_key_id,
                                    secret_access_key,
                                    region,
                                }
                            },
                            _ => OriginKind::Pax(url.clone()),
                        };
                        sources.push(origin);
                    } else if url.starts_with("apt://") {
                        sources.push(OriginKind::Apt(
                            url.strip_prefix("apt://").unwrap().to_string(),
                        ));
                    } else if url.starts_with("deb://") {
                        sources.push(OriginKind::Deb(
                            url.strip_prefix("deb://").unwrap().to_string(),
                        ));
                    } else if url.starts_with("yum://") || url.starts_with("dnf://") {
                        sources.push(OriginKind::Yum(
                            url.strip_prefix("yum://").or_else(|| url.strip_prefix("dnf://")).unwrap().to_string(),
                        ));
                    } else if url.starts_with("r2://") {
                        // Parse R2 URL format: r2://bucket.account_id.region
                        let parts: Vec<&str> = url.trim_start_matches("r2://").split('.').collect();
                        if parts.len() >= 2 {
                            let bucket = parts[0].to_string();
                            let account_id = parts[1].to_string();
                            let region = if parts.len() > 2 { Some(parts[2].to_string()) } else { None };
                            
                            sources.push(OriginKind::CloudflareR2 {
                                bucket,
                                account_id,
                                access_key_id: None,
                                secret_access_key: None,
                                region,
                            });
                        } else {
                            println!(
                                "\x1B[93m[WARN] Invalid R2 URL format on line {} of {}: {}\x1B[0m",
                                idx + 1,
                                path.display(),
                                url
                            );
                        }
                    } else if url.starts_with("github://") {
                        if let Some((user, repo)) =
                            url.trim_start_matches("github://").split_once('/')
                        {
                            sources.push(OriginKind::Github {
                                user: user.to_string(),
                                repo: repo.to_string(),
                            });
                        } else {
                            println!(
                                "\x1B[93m[WARN] Invalid GitHub URL `{}` on line {} of {}.\x1B[0m",
                                url,
                                idx + 1,
                                path.display()
                            );
                        }
                    } else {
                        println!(
                            "\x1B[93m[WARN] Unsupported repo URL `{}` on line {} of {}.\x1B[0m",
                            url,
                            idx + 1,
                            path.display()
                        );
                    }
                } else {
                    let github_pair = find("github")
                        .and_then(|value| value.split_once('/'))
                        .map(|(user, repo)| (user.to_string(), repo.to_string()))
                        .or_else(|| {
                            if provider.as_deref() == Some("github") {
                                if let (Some(user), Some(repo)) = (find("user"), find("repo")) {
                                    return Some((user.to_string(), repo.to_string()));
                                }
                            }
                            None
                        });

                    if let Some((user, repo)) = github_pair {
                        sources.push(OriginKind::Github { user, repo });
                    } else {
                        println!(
                            "\x1B[93m[WARN] Repository entry missing url= on line {} of {}.\x1B[0m",
                            idx + 1,
                            path.display()
                        );
                    }
                }
            }
            Some(other) => {
                println!(
                    "\x1B[93m[WARN] Unknown source type `{}` on line {} of {}.\x1B[0m",
                    other,
                    idx + 1,
                    path.display()
                );
            }
            None => {
                println!(
                    "\x1B[93m[WARN] Missing sourcetype= on line {} of {}.\x1B[0m",
                    idx + 1,
                    path.display()
                );
            }
        };
    }
    Ok((mirror, sources))
}

fn affirm_path() -> Result<PathBuf, String> {
    let mut path = get_dir()?;
    path.push("settings.yaml");
    if !path.exists() {
        match File::create(&path) {
            Ok(mut file) => {
                if let Ok(new_settings) = serde_norway::to_string(&SettingsYaml::new()) {
                    if file.write_all(new_settings.as_bytes()).is_ok() {
                        Ok(path)
                    } else {
                        err!("Failed to write to file!")
                    }
                } else {
                    err!("Failed to serialize settings!")
                }
            }
            Err(_) => err!("Failed to create settings file!"),
        }
    } else if path.is_file() {
        if File::open(&path).is_ok() {
            Ok(path)
        } else {
            err!("Failed to read settings file!")
        }
    } else {
        err!("Settings file is of unexpected type!")
    }
}

pub fn acquire_lock() -> Result<Option<PostAction>, String> {
    if !is_root() {
        return Ok(Some(PostAction::Elevate));
    }
    let mut settings = SettingsYaml::get_settings()?;
    let mut attempts = 0;
    const MAX_ATTEMPTS: i32 = 10; // Give up after 10 attempts (50 seconds total)
    
    loop {
        if settings.locked {
            attempts += 1;
            
            if attempts >= MAX_ATTEMPTS {
                // Force unlock and continue - better than hanging forever
                eprintln!("\x1B[93m[WARN] Forcing unlock after timeout (previous instance likely crashed).\x1B[0m");
                let mut tmp_settings = SettingsYaml::get_settings()?;
                tmp_settings.locked = false;
                tmp_settings.set_settings()?;
                break;
            }
            
            for i in 0..20 {
                print!(
                    "\x1B[2K\r\x1B[91mAwaiting program lock. Retrying in {:.2}s...\x1B[0m",
                    (100 - i) as f32 / 20f32
                );
                let _ = std::io::stdout().flush();
                sleep(Duration::from_millis(50));
            }
            for i in 0..20 {
                print!(
                    "\x1B[2K\r\x1B[93mAwaiting program lock. Retrying in {:.2}s\x1B[0m...",
                    (80 - i) as f32 / 20f32
                );
                let _ = std::io::stdout().flush();
                sleep(Duration::from_millis(50));
            }
            for i in 0..20 {
                print!(
                    "\x1B[2K\r\x1B[95mAwaiting program lock. Retrying in {:.2}s\x1B[0m...",
                    (60 - i) as f32 / 20f32
                );
                let _ = std::io::stdout().flush();
                sleep(Duration::from_millis(50));
            }
            for i in 0..20 {
                print!(
                    "\x1B[2K\r\x1B[94mAwaiting program lock. Retrying in {:.2}s\x1B[0m...",
                    (40 - i) as f32 / 20f32
                );
                let _ = std::io::stdout().flush();
                sleep(Duration::from_millis(50));
            }
            for i in 0..20 {
                print!(
                    "\x1B[2K\r\x1B[92mAwaiting program lock. Retrying in {:.2}s\x1B[0m...",
                    (20 - i) as f32 / 20f32
                );
                let _ = std::io::stdout().flush();
                sleep(Duration::from_millis(50));
            }
            println!("\x1B[2K\r\x1B[92mAwaiting program lock. Retrying now\x1B[0m...");
            settings = SettingsYaml::get_settings()?;
        } else {
            break;
        }
    }
    if settings.sources.is_empty() && settings.mirror_list.is_none() {
        return Ok(Some(PostAction::PullSources));
    }
    settings.locked = true;
    settings.set_settings()?;
    Ok(None)
}

pub fn remove_lock() -> Result<(), String> {
    let mut settings = SettingsYaml::get_settings()?;
    settings.locked = false;
    settings.set_settings()
}
