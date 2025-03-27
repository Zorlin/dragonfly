mod error;
pub mod models;

pub use error::Error;
pub use models::*;

pub type Result<T> = std::result::Result<T, Error>; 