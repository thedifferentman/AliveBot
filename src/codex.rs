//! AliveBot's local adapter for the independent Codex app-server service.
use crate::Config;
use anyhow::{Context, Result, ensure};
use reqwest::{Client, Method};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{path::Path, sync::OnceLock, time::Duration};

static CLIENT: OnceLock<Codex> = OnceLock::new();
pub fn client() -> &'static Codex {
    CLIENT.get().expect("Codex initialized at startup")
}

pub async fn init(config: &Config) -> Result<()> {
    let client = Codex::new(config)?;
    client.get("/health").await?;
    CLIENT
        .set(client)
        .map_err(|_| anyhow::anyhow!("Codex already initialized"))?;
    Ok(())
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Prompt {
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<Attachment>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attachment {
    pub uri: String,
    pub name: String,
}
impl Prompt {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            files: vec![],
        }
    }
    pub fn append(&mut self, mut other: Self) {
        let offset = self.files.len();
        for (index, file) in other.files.iter_mut().enumerate().rev() {
            let name = format!("image-{}", offset + index + 1);
            other.text = other.text.replace(
                &format!("<image:{}>", file.name),
                &format!("<image:{name}>"),
            );
            file.name = name;
        }
        self.text.push_str(&other.text);
        self.files.extend(other.files);
    }
}

pub struct Codex {
    http: Client,
    url: String,
    token: String,
    model: String,
    effort: String,
    fast_mode: bool,
}
impl Codex {
    pub fn new(config: &Config) -> Result<Self> {
        ensure!(!config.model.trim().is_empty(), "model cannot be empty");
        ensure!(
            ["none", "minimal", "low", "medium", "high", "xhigh", "max"]
                .contains(&config.reasoning_effort.as_str()),
            "unsupported reasoning_effort"
        );
        let token = std::env::var("ALIVEBOT_CODEX_TOKEN")
            .ok()
            .filter(|t| !t.is_empty())
            .map(Ok)
            .unwrap_or_else(|| std::fs::read_to_string(&config.codex_token_file))
            .context("Cannot read AliveBot Codex token; start codex-service/server.mjs first")?;
        Ok(Self {
            http: Client::builder()
                .no_proxy()
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(40))
                .build()?,
            url: config.codex_url.trim_end_matches('/').into(),
            token: token.trim().into(),
            model: config.model.clone(),
            effort: config.reasoning_effort.clone(),
            fast_mode: config.fast_mode,
        })
    }
    async fn request(&self, method: Method, route: &str, body: Option<&Value>) -> Result<Value> {
        let mut request = self
            .http
            .request(method, format!("{}{}", self.url, route))
            .bearer_auth(&self.token);
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request
            .send()
            .await
            .context("AliveBot Codex service connection failed")?;
        let status = response.status();
        let value: Value = response
            .json()
            .await
            .context("Invalid Codex adapter response")?;
        ensure!(
            status.is_success(),
            "Codex adapter HTTP {status}: {}",
            value["error"].as_str().unwrap_or("request failed")
        );
        Ok(value)
    }
    async fn get(&self, route: &str) -> Result<Value> {
        self.request(Method::GET, route, None).await
    }
    async fn post(&self, route: &str, body: &Value) -> Result<Value> {
        self.request(Method::POST, route, Some(body)).await
    }
    pub async fn wait_for_model(&self, _: &Path) -> Result<()> {
        ensure!(
            self.get("/health").await?["ready"] == true,
            "Codex service is not ready"
        );
        Ok(())
    }
    pub async fn ensure_session(
        &self,
        id: &str,
        directory: &Path,
        may_create: bool,
    ) -> Result<String> {
        let config = crate::CONFIG
            .get()
            .context("Missing AliveBot configuration")?;
        let result = self
            .post(
                "/sessions/ensure",
                &json!({"id": id, "directory": directory,
            "mayCreate": may_create, "model": self.model, "effort": self.effort,
            "instructions": config.live_system_prompt()?}),
            )
            .await?;
        Ok(result["threadId"]
            .as_str()
            .context("Codex thread ID missing")?
            .into())
    }
    pub async fn submit(
        &self,
        session: &str,
        id: &str,
        prompt: &Prompt,
        resume: bool,
    ) -> Result<()> {
        self.post(
            &format!("/sessions/{session}/prompt"),
            &json!({"id": id, "prompt": prompt, "resume": resume, "fastMode": self.fast_mode}),
        )
        .await?;
        Ok(())
    }
    pub async fn active(&self, session: &str) -> Result<bool> {
        Ok(self.get(&format!("/sessions/{session}/active")).await?["active"] == true)
    }
    pub async fn interrupt(&self, session: &str) -> Result<()> {
        if session.is_empty() {
            return Ok(());
        }
        self.post(&format!("/sessions/{session}/interrupt"), &json!({}))
            .await?;
        Ok(())
    }
    pub async fn history(&self, session: &str, after: u64) -> Result<Value> {
        self.get(&format!("/sessions/{session}/history?after={after}"))
            .await
    }
    pub async fn message(&self, session: &str, id: &str) -> Result<Value> {
        Ok(self
            .get(&format!("/sessions/{session}/message/{id}"))
            .await?["data"]
            .clone())
    }
}

pub fn error_summary(message: &Value) -> String {
    message["error"]["message"]
        .as_str()
        .unwrap_or("Codex generation failed")
        .chars()
        .take(300)
        .collect()
}
pub fn final_text(message: &Value) -> Option<String> {
    if message["type"] != "assistant"
        || !message["error"].is_null()
        || !matches!(
            message["finish"].as_str(),
            Some("stop" | "end-turn" | "end_turn" | "length")
        )
    {
        return None;
    }
    let content = message["content"].as_array()?;
    if content.iter().any(|p| p["type"] == "tool") {
        return None;
    }
    let text = content
        .iter()
        .filter(|p| p["type"] == "text")
        .filter_map(|p| p["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    (!text.trim().is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_public_text_is_deliverable() {
        for phase in ["commentary", "final_answer"] {
            assert_eq!(
                final_text(&json!({"type":"assistant","finish":"stop","phase":phase,
                "content":[{"type":"text","text":"hello"}]}))
                .as_deref(),
                Some("hello")
            );
        }
        assert!(final_text(&json!({"type":"assistant","finish":"stop","content":[{"type":"reasoning","text":"private"}]})).is_none());
        assert!(
            final_text(
                &json!({"type":"assistant","finish":"error","error":{"message":"bad"},"content":[]})
            )
            .is_none()
        );
    }
}
