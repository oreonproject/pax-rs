use serde::{Deserialize, Serialize};

pub mod apt;
pub mod github;
pub mod pax;
pub mod rpm;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum MetaDataKind {
    Apt,
    Pax,
    Github,
    Rpm,
}
