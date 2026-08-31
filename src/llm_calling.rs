use crate::CONFIG;
use crate::context_manage::Context;
use anyhow::Result;
use reqwest::{Client, RequestBuilder};
use serde_json::Value;
use std::time::Duration;
use tracing::info;

fn http_client() -> Result<Client> {
    Ok(Client::builder()
        .timeout(Duration::from_secs(300))
        .build()?)
}

fn post(client: &Client, path: &str) -> RequestBuilder {
    let config = CONFIG.get().unwrap();
    let request = client.post(format!("{}{}", config.openai_url, path));
    if config.api_key.is_empty() {
        request
    } else {
        request.bearer_auth(&config.api_key)
    }
}

pub async fn request_llamacpp(data: &Context) -> Result<String> {
    let data: Value = data.into();
    let client = http_client()?;
    info!("post:\n{data}");
    let response = post(&client, "/chat/completions")
        .json(&data)
        .send()
        .await?;
    let data: Value = response.json().await?;
    info!("response:\n{data}");
    Ok(data["choices"][0]["message"]["content"]
        .as_str()
        .unwrap()
        .to_string())
}

pub async fn request_token_count(data: &Context) -> Result<u64> {
    let data: Value = data.into();
    let client = http_client()?;
    let response = post(&client, "/chat/completions/input_tokens")
        .json(&data)
        .send()
        .await?;
    let data: Value = response.json().await?;
    Ok(data["input_tokens"].as_u64().unwrap())
}
