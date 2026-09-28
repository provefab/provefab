#[derive(Debug, thiserror::Error)]
pub enum JevError {
    #[error("jev: unauthorized (check TYPESAFE_API_KEY)")]
    Unauthorized,
    #[error("jev: request rejected: {0}")]
    Invalid(String),
    #[error("jev: rate limited or overloaded after retries")]
    RateLimited,
    #[error("jev: timed out")]
    Timeout,
    #[error("jev: http {status}: {body}")]
    Http { status: u16, body: String },
    #[error("jev: transport: {0}")]
    Transport(String),
    #[error("jev: unreadable response body: {0}")]
    Decode(String),
    #[error("jev: answer `{0}` missing from response")]
    MissingAnswer(String),
    #[error("jev: answer `{id}` is not a {expected}")]
    WrongType { id: String, expected: &'static str },
}
