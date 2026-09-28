use std::time::Duration;

use serde_json::Value;

use crate::JevError;
use crate::types::{Questions, Request, Response};

pub const DEFAULT_BASE_URL: &str = "https://api.typesafe.ai";

/// Backoff before each retry of a 429/529, per TypeSafe's "retry with exponential backoff".
const RETRY_DELAYS: [Duration; 2] = [Duration::from_millis(100), Duration::from_millis(300)];

pub struct JevClient {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
}

impl JevClient {
    /// `timeout` bounds each HTTP attempt. Provefab passes 2s (spec §4).
    pub fn new(
        api_key: impl Into<String>,
        model: impl Into<String>,
        timeout: Duration,
    ) -> Result<Self, JevError> {
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| JevError::Transport(e.to_string()))?;
        Ok(Self {
            http,
            base_url: DEFAULT_BASE_URL.to_string(),
            api_key: api_key.into(),
            model: model.into(),
        })
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub async fn evaluate(
        &self,
        state: &Value,
        questions: &Questions,
    ) -> Result<Response, JevError> {
        let url = format!("{}/v1/systemone", self.base_url.trim_end_matches('/'));
        let body = Request {
            model: &self.model,
            state,
            questions,
        };
        let mut retries = RETRY_DELAYS.iter();
        loop {
            let res = self
                .http
                .post(&url)
                .bearer_auth(&self.api_key)
                .json(&body)
                .send()
                .await
                .map_err(transport)?;
            let status = res.status().as_u16();
            match status {
                200..=299 => {
                    let text = res.text().await.map_err(transport)?;
                    return serde_json::from_str(&text)
                        .map_err(|e| JevError::Decode(e.to_string()));
                }
                429 | 529 => match retries.next() {
                    Some(delay) => tokio::time::sleep(*delay).await,
                    None => return Err(JevError::RateLimited),
                },
                401 => return Err(JevError::Unauthorized),
                422 => return Err(JevError::Invalid(res.text().await.unwrap_or_default())),
                _ => {
                    return Err(JevError::Http {
                        status,
                        body: res.text().await.unwrap_or_default(),
                    });
                }
            }
        }
    }
}

fn transport(e: reqwest::Error) -> JevError {
    if e.is_timeout() {
        JevError::Timeout
    } else {
        JevError::Transport(e.to_string())
    }
}
