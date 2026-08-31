use crate::CONFIG;
use crate::llm_calling::request_token_count;
use anyhow::{Result, anyhow};
use nagisa::prelude::*;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use tokio::sync::Mutex;

#[derive(Debug)]
pub struct Context {
    config: Value,
    system: String,
    ctx: Vec<Value>,
}

pub static CONTEXT: OnceLock<Mutex<HashMap<Peer, Arc<Mutex<Context>>>>> = OnceLock::new();

impl Context {
    pub fn new() -> Self {
        let config = CONFIG.get().unwrap();
        Self {
            config: json!({
                "model": config.model,
                "temperature": config.temperature,
                "top_p": config.top_p,
                "top_k": config.top_k,
                "repeat_penalty": config.repeat_penalty,
                "max_tokens": config.max_tokens,
            }),
            system: config.system_prompt.clone(),
            ctx: vec![],
        }
    }

    pub fn clear(&mut self) {
        self.ctx.clear();
    }

    pub async fn push(&mut self, content: Value) {
        self.ctx.push(content);
    }

    pub async fn extend(&mut self, content: Vec<Value>) {
        self.ctx.extend(content);
    }

    pub async fn cut(&mut self) -> Result<()> {
        while request_token_count(self).await?
            > (CONFIG.get().unwrap().max_tokens as f32 * 0.9) as u64
        {
            if self.ctx.is_empty() {
                return Err(anyhow!("Context is empty"));
            }
            self.ctx.remove(0);
        }
        Ok(())
    }
}

impl Into<Value> for &Context {
    fn into(self) -> Value {
        let mut result = self.config.clone();
        result.as_object_mut().unwrap().insert(
            "messages".to_string(),
            json!([
                {"role":"system","content":self.system.clone()},
                {"role":"user","content":self.ctx.clone()}
            ]),
        );
        result
    }
}
