use serde::{Deserialize, Serialize};

pub mod pax;

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum MetaDataKind {
    Pax,
}
