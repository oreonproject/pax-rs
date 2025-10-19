use serde::{Deserialize, Serialize};

pub mod pax;

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum MetaDataKind {
    Pax,
}

impl std::fmt::Display for MetaDataKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self {
            MetaDataKind::Pax => write!(f, "pax"),
        }
    }
}
