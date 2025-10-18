use std::{cmp::Ordering, fs::DirBuilder, io::Write, path::PathBuf, process::Command};

use flags::Flag;
use nix::unistd;
use serde::{Deserialize, Serialize};

// The action to perform once a command has run
pub enum PostAction {
    Elevate,
    Err(i32),
    Fuck(String),
    GetHelp,
    NothingToDo,
    PullSources,
    Return,
}

pub fn get_dir() -> Result<PathBuf, String> {
    let path = PathBuf::from("/etc/pax");
    if !path.exists() && DirBuilder::new().create(&path).is_err() {
        err!("Failed to create pax directory!")
    } else {
        Ok(path)
    }
}

pub fn get_metadata_dir() -> Result<PathBuf, String> {
    let mut path = get_dir()?;
    path.push("installed");
    if !path.exists() && DirBuilder::new().create(&path).is_err() {
        err!("Failed to create pax installation directory!")
    } else {
        Ok(path)
    }
}

pub fn get_update_dir() -> Result<PathBuf, String> {
    let mut path = get_dir()?;
    path.push("updates");
    if !path.exists() && DirBuilder::new().create(&path).is_err() {
        err!("Failed to create pax installation directory!")
    } else {
        Ok(path)
    }
}

pub fn is_root() -> bool {
    unistd::geteuid().as_raw() == 0
}

pub fn tmpfile() -> Option<PathBuf> {
    Some(PathBuf::from(
        String::from_utf8_lossy(&Command::new("mktemp").output().ok()?.stdout).trim(),
    ))
}

pub fn yes_flag() -> Flag {
    Flag::new(
        Some('y'),
        "yes",
        "Bypasses applicable confirmation dialogs.",
        false,
        false,
        |states, _| {
            states.shove("yes", true);
        },
    )
}

pub fn specific_flag() -> Flag {
    Flag::new(
        Some('s'),
        "specific",
        "Makes every second argument the target version for the argument prior.",
        false,
        false,
        |states, _| {
            states.shove("specific", true);
        },
    )
}

// I learned this basic macro from Kernel dev
#[macro_export]
macro_rules! err {
    ($fmt:literal $(, $args:expr)*) => {Err(format!($fmt $(, $args)*))};
}

pub fn choice(message: &str, default_yes: bool) -> Result<bool, String> {
    print!(
        "{} [{}]: ",
        message,
        if default_yes { "Y/n" } else { "y/N" }
    );
    let _ = std::io::stdout().flush();
    let mut input = String::new();
    if std::io::stdin().read_line(&mut input).is_err() {
        return err!("\nFailed to read terminal input!");
    }
    if default_yes {
        if ["no", "n", "false", "f"].contains(&input.to_lowercase().trim()) {
            Ok(false)
        } else {
            Ok(true)
        }
    } else if ["yes", "y", "true", "t"].contains(&input.to_lowercase().trim()) {
        Ok(true)
    } else {
        Ok(false)
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct Version {
    pub major: usize,
    pub minor: usize,
    pub patch: usize,
    pub pre: String,
}

impl Version {
    pub fn parse(src: &str) -> Result<Self, String> {
        let src = src.split_once('+').map(|x| x.0).unwrap_or(src);
        let (src, pre) = src
            .split_once('-')
            .map(|x| (x.0, x.1.to_string()))
            .unwrap_or_else(|| (src, String::new()));
        let split = src.split('.').collect::<Vec<&str>>();
        if !split.is_empty() {
            if let Ok(major) = split[0].parse::<usize>() {
                if split.len() >= 2 {
                    if let Ok(minor) = split[1].parse::<usize>() {
                        if split.len() >= 3 {
                            if let Ok(patch) = split[2].parse::<usize>() {
                                if split.len() > 3 {
                                    err!("Two many segments in version!")
                                } else {
                                    Ok(Self {
                                        major,
                                        minor,
                                        patch,
                                        pre,
                                    })
                                }
                            } else {
                                err!("Expected patch to be a number, got `{}`!", split[1])
                            }
                        } else {
                            Ok(Self {
                                major,
                                minor,
                                patch: 0,
                                pre,
                            })
                        }
                    } else {
                        err!("Expected minor to be a number, got `{}`!", split[1])
                    }
                } else {
                    Ok(Self {
                        major,
                        minor: 0,
                        patch: 0,
                        pre,
                    })
                }
            } else {
                err!("Expected major to be a number, got `{}`!", split[0])
            }
        } else {
            err!("A version must be specified!")
        }
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.pre.is_empty() {
            f.write_str(&format!("{}.{}.{}", self.major, self.minor, self.patch))
        } else {
            f.write_str(&format!(
                "{}.{}.{}-{}",
                self.major, self.minor, self.patch, self.pre
            ))
        }
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        match self.major.cmp(&other.major) {
            Ordering::Equal => match self.minor.cmp(&other.minor) {
                Ordering::Equal => match self.patch.cmp(&other.patch) {
                    Ordering::Equal => self.pre.cmp(&other.pre),
                    order => order,
                },
                order => order,
            },
            order => order,
        }
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum VerReq {
    Gt(Version),
    Ge(Version),
    Eq(Version),
    Le(Version),
    Lt(Version),
    NoBound,
}

impl VerReq {
    pub fn negotiate(&self, prior: Option<Range>) -> Option<Range> {
        let prior = if let Some(mut prior) = prior {
            match self {
                Self::Gt(gt) => match &prior.lower {
                    Self::Gt(p_gt) => {
                        if gt > p_gt {
                            prior.lower = self.clone();
                        }
                        Some(prior)
                    }
                    Self::Ge(p_ge) => {
                        if gt >= p_ge {
                            prior.lower = self.clone();
                        }
                        Some(prior)
                    }
                    Self::NoBound => {
                        prior.lower = self.clone();
                        Some(prior)
                    }
                    _ => None,
                },
                Self::Ge(ge) => match &prior.lower {
                    Self::Gt(p_gt) => {
                        if ge > p_gt {
                            prior.lower = self.clone();
                        }
                        Some(prior)
                    }
                    Self::Ge(p_ge) => {
                        if ge > p_ge {
                            prior.lower = self.clone();
                        }
                        Some(prior)
                    }
                    Self::Eq(p_eq) => {
                        if ge == p_eq {
                            Some(prior)
                        } else {
                            None
                        }
                    }
                    Self::NoBound => {
                        prior.lower = self.clone();
                        Some(prior)
                    }
                    _ => None,
                },
                Self::Eq(eq) => {
                    match &prior.lower {
                        Self::Gt(p_gt) => {
                            if eq > p_gt {
                                prior.lower = self.clone();
                            } else {
                                return None;
                            }
                        }
                        Self::Ge(p_ge) => {
                            if eq >= p_ge {
                                prior.lower = self.clone();
                            } else {
                                return None;
                            }
                        }
                        Self::Eq(p_eq) => {
                            if eq != p_eq {
                                return None;
                            }
                        }
                        Self::NoBound => {
                            prior.lower = self.clone();
                        }
                        _ => return None,
                    }
                    match &prior.upper {
                        Self::Eq(p_eq) => {
                            if eq != p_eq {
                                return None;
                            }
                        }
                        Self::Le(p_le) => {
                            if eq <= p_le {
                                prior.upper = self.clone();
                            } else {
                                return None;
                            }
                        }
                        Self::Lt(p_lt) => {
                            if eq < p_lt {
                                prior.upper = self.clone();
                            } else {
                                return None;
                            }
                        }
                        Self::NoBound => {
                            prior.upper = self.clone();
                        }
                        _ => return None,
                    }
                    Some(prior)
                }
                Self::Le(le) => match &prior.upper {
                    Self::Lt(p_lt) => {
                        if le < p_lt {
                            prior.upper = self.clone();
                        }
                        Some(prior)
                    }
                    Self::Le(p_le) => {
                        if le < p_le {
                            prior.upper = self.clone();
                        }
                        Some(prior)
                    }
                    Self::Eq(p_eq) => {
                        if le == p_eq {
                            Some(prior)
                        } else {
                            None
                        }
                    }
                    Self::NoBound => {
                        prior.upper = self.clone();
                        Some(prior)
                    }
                    _ => None,
                },
                Self::Lt(lt) => match &prior.upper {
                    Self::Lt(p_lt) => {
                        if lt < p_lt {
                            prior.upper = self.clone();
                        }
                        Some(prior)
                    }
                    Self::Le(p_le) => {
                        if lt <= p_le {
                            prior.upper = self.clone();
                        }
                        Some(prior)
                    }
                    Self::NoBound => {
                        prior.upper = self.clone();
                        Some(prior)
                    }
                    _ => None,
                },
                Self::NoBound => None,
            }
        } else {
            None
        };
        if prior.as_ref().is_some_and(|x| x.is_sane()) {
            prior
        } else {
            None
        }
    }
    // pub fn to_string(&self) -> Option<String> {
    //     match self {
    //         Self::Gt(s) | Self::Ge(s) | Self::Eq(s) | Self::Le(s) | Self::Lt(s) => {
    //             Some(s.to_string())
    //         }
    //         Self::NoBound => None,
    //     }
    // }
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct Range {
    pub lower: VerReq,
    pub upper: VerReq,
}

impl Range {
    pub fn is_sane(&self) -> bool {
        match &self.lower {
            VerReq::Gt(gt) => match &self.upper {
                VerReq::Eq(o_eq) => gt == o_eq,
                VerReq::Le(o) | VerReq::Lt(o) => gt < o,
                VerReq::NoBound => true,
                _ => false,
            },
            VerReq::Ge(ge) => match &self.upper {
                VerReq::Eq(o_eq) => ge == o_eq,
                VerReq::Le(o_le) => ge <= o_le,
                VerReq::Lt(o_lt) => ge < o_lt,
                VerReq::NoBound => true,
                _ => false,
            },
            VerReq::Eq(eq) => match &self.upper {
                VerReq::Eq(o_eq) => eq == o_eq,
                VerReq::NoBound => true,
                _ => false,
            },
            VerReq::NoBound => true,
            _ => false,
        }
    }
}
