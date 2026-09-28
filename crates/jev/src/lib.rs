//! Typed client for TypeSafe's System One API (`POST /v1/systemone`).
//!
//! Knows nothing about Provefab: questions in, typed answers out.

mod client;
mod error;
mod types;

pub use client::{DEFAULT_BASE_URL, JevClient};
pub use error::JevError;
pub use types::{
    Answer, ChoiceAnswer, NoulAnswer, NoulCriteria, Question, Questions, Response, ScoreAnswer,
    Usage,
};
