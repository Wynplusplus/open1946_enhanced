//! Parsers for IL-2 1946 asset formats. Specifications live in `docs/formats/`.

pub mod binsect;
pub mod finger;
pub mod him;
pub mod imf;
pub mod map;
pub mod mat;
pub mod mission;
pub mod msh;
pub mod ntrk;
pub mod ntrk_msg;
pub mod ntrk_traffic;
pub mod properties;
pub mod sectfile;
pub mod statics;
pub mod tga;

pub use binsect::{RowFile, RowSection, Value};
pub use sectfile::{SectFile, Section};
