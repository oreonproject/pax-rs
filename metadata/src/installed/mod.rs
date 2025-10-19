use serde::{Deserialize, Serialize};
use settings::OriginKind;
use std::{
    fs::File,
    io::{Read, Write},
    path::Path,
};
use utils::{err, get_metadata_dir};

use crate::processed::PreBuilt;
use crate::{DepVer, MetaDataKind, Specific};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct InstalledMetaData {
    pub locked: bool,
    pub name: String,
    pub kind: MetaDataKind,
    pub version: String,
    pub origin: OriginKind,
    pub dependent: bool,
    pub dependencies: Vec<DepVer>,
    pub dependents: Vec<Specific>,
    pub install_kind: InstalledInstallKind,
    pub hash: String,
}

impl InstalledMetaData {
    pub fn open(name: &str) -> Result<Self, String> {
        let mut path = get_metadata_dir()?;
        path.push(format!("{}.yaml", name));
        let mut file = match File::open(&path) {
            Ok(file) => file,
            Err(_) => return err!("Failed to read package `{name}`'s metadata!"),
        };
        let mut metadata = String::new();
        if file.read_to_string(&mut metadata).is_err() {
            return err!("Failed to read package `{name}`'s config!");
        }
        Ok(match serde_norway::from_str::<Self>(&metadata) {
            Ok(data) => data,
            Err(_) => return err!("Failed to parse package `{name}`'s data!"),
        })
    }
    pub fn write(self, path: &Path) -> Result<Option<Self>, String> {
        if !path.exists() || path.is_file() {
            let data = match serde_norway::to_string(&self) {
                Ok(data) => data,
                Err(_) => {
                    return err!("Failed to parse InstalledMetaData into string!");
                }
            };
            let mut file = match File::create(path) {
                Ok(file) => file,
                Err(_) => return err!("Failed to open file as WO!"),
            };
            match file.write_all(data.as_bytes()) {
                Ok(_) => Ok(Some(self)),
                Err(_) => err!("Failed to write to file!"),
            }
        } else {
            err!("File is of unexpected type!")
        }
    }
    pub fn lock(mut self, path: &Path, name: &str) -> Result<Option<Self>, String> {
        self.locked = true;
        if let Some(data) = self.write(path)? {
            Ok(Some(data))
        } else {
            println!(
                "\x1B[33m[WARN] Skipping `{}` as it has no dependencies.\x1B[0m",
                name
            );
            println!(
                "\x1B[91m=== THIS IS UNEXPECTED BEHAVIOR, AND USUALLY INDICATES BROKEN PACKAGES! ===\x1B[0m..."
            );
            Ok(None)
        }
    }
    pub fn clear_dependencies(&self, specific: &Specific) -> Result<(), String> {
        let mut path = get_metadata_dir()?;
        let mut data = self.clone();
        let Some(index) = &data
            .dependencies
            .iter()
            .position(|x| x.get_installed_specific().is_ok_and(|x| x == *specific))
        else {
            return err!(
                "`{}` {} didn't contain dependent `{}`!",
                data.name.to_string(),
                data.version.to_string(),
                specific.name
            );
        };
        data.dependencies.remove(*index);
        path.push(format!("{}.yaml", self.name));
        data.write(&path)?;
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub enum InstalledInstallKind {
    PreBuilt(PreBuilt),
    Compilable(InstalledCompilable),
}
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct InstalledCompilable {
    pub uninstall: String,
    pub purge: String,
}
