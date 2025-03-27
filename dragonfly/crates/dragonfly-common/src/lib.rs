pub mod error;
pub mod models;
pub mod mac_to_words;

pub use error::Error;
pub use models::*;

pub type Result<T> = std::result::Result<T, Error>; 