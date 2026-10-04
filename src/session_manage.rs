//! Per-group transport state. Model conversation history lives in Codex.
use crate::{
    CONFIG, Config, actions,
    codex::{self, Prompt},
    events,
};
use anyhow::{Context, Result, ensure};
use nagisa::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
#[cfg(test)]
use serde_json::json;
use std::{
    collections::{HashMap, VecDeque},
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{Mutex, Notify};
use tracing::{error, info, warn};

static GROUPS: OnceLock<Mutex<HashMap<Peer, Arc<Group>>>> = OnceLock::new();

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Input {
    id: String,
    prompt: Prompt,
    resume: bool,
    own: bool,
    #[serde(default)]
    retry: bool,
    #[serde(default)]
    not_before_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Candidate {
    id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SentMessage {
    id: i32,
    sender: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SentFile {
    id: String,
    sender: i64,
}

const SENT_MESSAGE_LIMIT: usize = 4096;

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Stored {
    session_id: String,
    backend: String,
    thread_id: String,
    created: bool,
    cursor: u64,
    pending: VecDeque<Input>,
    seen: VecDeque<String>,
    sent_messages: VecDeque<SentMessage>,
    sent_files: VecDeque<SentFile>,
    // Legacy single-response state, migrated on load.
    #[serde(skip_serializing_if = "Option::is_none")]
    candidate: Option<Candidate>,
    completed: VecDeque<Candidate>,
    // Persist before calling QQ. An interrupted send must not be blindly repeated.
    sending: Option<Candidate>,
    retry_attempts: u8,
    retry_eligible_run: bool,
    run_has_activity: bool,
}

pub struct Group {
    pub ingress: Mutex<()>,
    bot: Bot,
    peer: Peer,
    directory: PathBuf,
    state: Mutex<Stored>,
    notify: Notify,
}

fn new_id(prefix: &str) -> String {
    format!("{prefix}_{:032x}", rand::random::<u128>())
}

pub async fn get(bot: Bot, peer: Peer) -> Result<Arc<Group>> {
    let mut groups = GROUPS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .await;
    if let Some(group) = groups.get(&peer) {
        return Ok(group.clone());
    }
    let config = CONFIG.get().unwrap();
    let directory = prepare_directory(&config.workspace_dir, peer.id.0, config)?;
    let state_file = directory.join(".alivebot/session.json");
    let mut state = if state_file.exists() {
        serde_json::from_slice::<Stored>(&std::fs::read(&state_file)?)
            .context("Invalid saved session state; refusing to discard it")?
    } else {
        Stored::default()
    };
    if state.backend != "codex" {
        let backup = directory.join(".alivebot/opencode-session.backup.json");
        if state_file.exists() && !backup.exists() {
            std::fs::copy(&state_file, backup)?;
        }
        state.backend = "codex".into();
        state.session_id.clear();
        state.thread_id.clear();
        state.created = false;
        state.cursor = 0;
        state.completed.clear();
        state.candidate = None;
        state.sending = None;
        state.pending.retain(|input| !input.retry);
        state.retry_eligible_run = false;
        state.retry_attempts = 0;
        state.run_has_activity = false;
    }
    if state.session_id.is_empty() {
        state.session_id = new_id("ses");
    }
    if let Some(candidate) = state.candidate.take() {
        if !state.completed.iter().any(|item| item.id == candidate.id) {
            state.completed.push_front(candidate);
        }
    }
    if let Some(uncertain) = state.sending.take() {
        warn!(
            group = peer.id.0,
            message = uncertain.id,
            "Previous QQ delivery outcome unknown; not resending"
        );
    }
    // Remove transport receipts queued by older versions; they are log events,
    // not chat inputs or additional instructions for the model.
    state
        .pending
        .retain(|input| !input.prompt.text.trim_start().starts_with("<delivery "));
    write_json(&state_file, &state)?;
    let group = Arc::new(Group {
        ingress: Mutex::new(()),
        bot,
        peer,
        directory,
        state: Mutex::new(state),
        notify: Notify::new(),
    });
    groups.insert(peer, group.clone());
    let worker = group.clone();
    tokio::spawn(async move { worker.run().await });
    Ok(group)
}

pub async fn restore(bot: Bot) -> Result<()> {
    let config = CONFIG.get().unwrap();
    // Restore only whitelisted groups with existing state, not every directory.
    for id in &config.group_whitelist {
        if config
            .workspace_dir
            .join(id.to_string())
            .join(".alivebot/session.json")
            .exists()
        {
            if let Err(err) = get(bot.clone(), Peer::group(Uin(*id))).await {
                error!(group = id, error = %err, "Cannot restore group; other groups will continue");
            }
        }
    }
    Ok(())
}

impl Group {
    fn save(&self, state: &Stored) -> Result<()> {
        write_json(&self.directory.join(".alivebot/session.json"), state)
    }

    pub async fn is_model_echo(&self, id: i32, sender: Uin) -> bool {
        // tick holds this lock through sending, recording the returned ID, and
        // saving it. An early NapCat report waits until that process completes.
        let state = self.state.lock().await;
        let matched = state.sent_messages.contains(&SentMessage {
            id,
            sender: sender.0,
        });
        if matched {
            info!(
                group = self.peer.id.0,
                message_id = id,
                "Model message echo ignored; assistant response already in context"
            );
        }
        matched
    }

    pub async fn is_model_file_echo(&self, id: &str, sender: Uin) -> bool {
        if id.is_empty() {
            return false;
        }
        let state = self.state.lock().await;
        let matched = state
            .sent_files
            .iter()
            .any(|file| file.id == id && file.sender == sender.0);
        if matched {
            info!(
                group = self.peer.id.0,
                file_id = id,
                "Model file upload echo ignored"
            );
        }
        matched
    }

    pub async fn enqueue(
        &self,
        prompt: Prompt,
        resume: bool,
        own: bool,
        event_key: Option<String>,
    ) -> Result<()> {
        let mut state = self.state.lock().await;
        if let Some(key) = event_key {
            if state.seen.contains(&key) {
                return Ok(());
            }
            state.seen.push_back(key);
            if state.seen.len() > 4096 {
                state.seen.pop_front();
            }
        }
        if resume && !own {
            state.pending.retain(|input| !input.retry);
        }
        state.pending.push_back(Input {
            retry: false,
            not_before_ms: 0,
            id: new_id("msg"),
            prompt,
            resume: resume && !own,
            own,
        });
        self.save(&state)?;
        drop(state);
        self.notify.notify_one();
        Ok(())
    }

    pub async fn reset(&self) -> Result<()> {
        let mut state = self.state.lock().await;
        // Waiting for cancellation also fences late replies from the previous session.
        codex::client().interrupt(&state.session_id).await?;
        let next = Stored {
            session_id: new_id("ses"),
            backend: "codex".into(),
            // Late echoes still belong to model sends even after /new.
            sent_messages: state.sent_messages.clone(),
            sent_files: state.sent_files.clone(),
            ..Stored::default()
        };
        self.save(&next)?;
        *state = next;
        drop(state);
        self.notify.notify_one();
        Ok(())
    }

    pub async fn stop(&self) -> Result<()> {
        let mut state = self.state.lock().await;
        codex::client().interrupt(&state.session_id).await?;
        if state.created {
            while !self.read_history(&mut state).await? {}
        }
        state.candidate = None;
        state.completed.clear();
        state.retry_eligible_run = false;
        state.pending.retain(|input| !input.retry);
        for input in &mut state.pending {
            input.resume = false;
        }
        state.pending.push_back(Input {
            retry: false, not_before_ms: 0,
            id: new_id("msg"), resume: false, own: true,
            prompt: Prompt::text("<control>用户已执行 /stop，取消此前尚未完成的任务。等待新请求，不要自动继续被取消的工作。</control>"),
        });
        self.save(&state)?;
        self.notify.notify_one();
        Ok(())
    }

    async fn run(self: Arc<Self>) {
        let mut initialized = String::new();
        loop {
            if let Err(err) = self.tick(&mut initialized).await {
                error!(group = self.peer.id.0, error = %err, "Codex group worker failed; retained transport state for retry");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
            tokio::select! {
                _ = self.notify.notified() => {},
                _ = tokio::time::sleep(Duration::from_secs(1)) => {},
            }
        }
    }

    async fn tick(&self, initialized: &mut String) -> Result<()> {
        let api = codex::client();
        let mut state = self.state.lock().await;
        if *initialized != state.session_id || !state.pending.is_empty() {
            api.wait_for_model(&self.directory).await?;
            state.thread_id = api
                .ensure_session(&state.session_id, &self.directory, !state.created)
                .await?;
            state.created = true;
            self.save(&state)?;
            if *initialized != state.session_id {
                *initialized = state.session_id.clone();
                info!(
                    group = self.peer.id.0,
                    thread = state.thread_id,
                    "Codex session ready"
                );
            }
        }
        let mut active = api.active(&state.session_id).await?;
        // Every valid report, including own non-model reports, steers an active turn.
        for _ in 0..32 {
            let now = now_ms();
            let Some(index) = state
                .pending
                .iter()
                .position(|item| item.not_before_ms <= now)
            else {
                break;
            };
            let input = &state.pending[index];
            api.submit(&state.session_id, &input.id, &input.prompt, input.resume)
                .await?;
            let resume = input.resume;
            let retry = input.retry;
            if resume && !active && !retry {
                state.retry_attempts = 0;
                state.retry_eligible_run = true;
                state.run_has_activity = false;
            }
            active |= resume;
            state.pending.remove(index);
            self.save(&state)?;
        }
        // Completed responses are durable and remain deliverable even while a
        // later steered task is running. Do not wait for the history backlog either.
        self.read_history(&mut state).await?;
        let Some(candidate) = state.completed.front().cloned() else {
            return Ok(());
        };
        let message = api.message(&state.session_id, &candidate.id).await?;
        // A historical failure must not restart work that has already continued.
        let retry_allowed = can_retry(&state, &message)
            && state.completed.len() == 1
            && !api.active(&state.session_id).await?;
        self.deliver_completed(&mut state, &candidate, &message, retry_allowed)
            .await?;
        if !state.completed.is_empty() {
            self.notify.notify_one();
        }
        Ok(())
    }

    async fn deliver_completed(
        &self,
        state: &mut Stored,
        candidate: &Candidate,
        message: &Value,
        retry_allowed: bool,
    ) -> Result<()> {
        let Some(text) = codex::final_text(&message) else {
            if !message["error"].is_null() {
                if retry_allowed {
                    state.retry_attempts += 1;
                    let delay = if state.retry_attempts == 1 {
                        2_000
                    } else {
                        5_000
                    };
                    state.pending.push_back(Input {
                        id: new_id("msg"), resume: true, own: false, retry: true,
                        not_before_ms: now_ms() + delay,
                        prompt: Prompt::text("<control>上一轮在产生输出或执行工具之前发生 HTTP 传输故障。请基于现有上下文继续处理最新请求；不要把此控制记录回复给群友。</control>"),
                    });
                    warn!(
                        group = self.peer.id.0,
                        message = candidate.id,
                        retry = state.retry_attempts,
                        delay_ms = delay,
                        "OpenCode HTTP transport failed before output/tools; retry scheduled"
                    );
                } else {
                    error!(group = self.peer.id.0, message = candidate.id,
                        cause = %codex::error_summary(&message), retries = state.retry_attempts,
                        "OpenCode generation failed");
                }
            } else {
                info!(
                    group = self.peer.id.0,
                    message = candidate.id,
                    finish = message["finish"].as_str().unwrap_or("unknown"),
                    reason = "no deliverable final text",
                    "Model reply skipped"
                );
            }
            state.completed.pop_front();
            self.save(&state)?;
            return Ok(());
        };
        state.completed.pop_front();
        state.sending = Some(candidate.clone());
        self.save(&state)?;
        let result =
            actions::send_message(self.bot.clone(), &text, self.peer, &self.directory).await;
        state.sending = None;
        match result {
            Ok(actions::Delivery::Read(request)) => {
                let (kind, id, result) = match request {
                    actions::ReadRequest::Unfold(id) => {
                        let result = events::forward_prompt(self.bot.clone(), self.peer, &id).await;
                        ("forward", id, result)
                    }
                    actions::ReadRequest::Get(id) => {
                        let result =
                            events::get_message_prompt(self.bot.clone(), self.peer, id).await;
                        ("get", id.to_string(), result)
                    }
                };
                let prompt = match result {
                    Ok(prompt) => prompt,
                    Err(_) => {
                        warn!(
                            group = self.peer.id.0,
                            operation = kind,
                            target = id,
                            "QQ message lookup unavailable"
                        );
                        Prompt::text(format!(
                            "<{kind} id:{id} error:unavailable>无法读取该消息，可能不存在、已失效或不属于当前群。不要编造内容或反复查询同一失败目标，请基于已有信息处理。</{kind}>"
                        ))
                    }
                };
                state.pending.push_back(Input {
                    retry: false,
                    not_before_ms: 0,
                    id: new_id("msg"),
                    prompt,
                    resume: true,
                    own: false,
                });
                self.notify.notify_one();
            }
            Ok(actions::Delivery::NoMessage) => {}
            Ok(actions::Delivery::Sent(id)) => {
                if id.peer == self.peer && id.onebot_id.is_some_and(|id| id != 0) {
                    remember_model_message(state, id.onebot_id.unwrap(), self.bot.self_id());
                } else {
                    warn!(
                        group = self.peer.id.0,
                        "QQ send succeeded without a usable message ID; cannot filter its echo"
                    );
                }
            }
            Ok(actions::Delivery::FileUploaded { id, name }) => {
                if id.is_empty() {
                    warn!(
                        group = self.peer.id.0,
                        "File uploaded without a file ID; cannot reliably filter its echo"
                    );
                } else {
                    remember_model_file(state, id, self.bot.self_id());
                }
                info!(group = self.peer.id.0, name, "File uploaded");
            }
            Err(err) => {
                warn!(group = self.peer.id.0, error = %err, "QQ delivery failed; not automatically retrying an ambiguous send");
            }
        }
        self.save(&state)?;
        Ok(())
    }

    async fn read_history(&self, state: &mut Stored) -> Result<bool> {
        let api = codex::client();
        for _ in 0..20 {
            let history = api.history(&state.session_id, state.cursor).await?;
            let entries = history["data"]
                .as_array()
                .context("Missing Codex delivery history data")?;
            let previous = state.cursor;
            for event in entries {
                apply_event(state, event);
            }
            if state.cursor != previous {
                self.save(state)?;
            }
            if history["hasMore"] != true {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

fn remember_model_message(state: &mut Stored, id: i32, sender: Uin) {
    let entry = SentMessage {
        id,
        sender: sender.0,
    };
    if !state.sent_messages.contains(&entry) {
        state.sent_messages.push_back(entry);
        while state.sent_messages.len() > SENT_MESSAGE_LIMIT {
            state.sent_messages.pop_front();
        }
    }
}

fn remember_model_file(state: &mut Stored, id: String, sender: Uin) {
    if state
        .sent_files
        .iter()
        .any(|file| file.id == id && file.sender == sender.0)
    {
        return;
    }
    state.sent_files.push_back(SentFile {
        id,
        sender: sender.0,
    });
    while state.sent_files.len() > SENT_MESSAGE_LIMIT {
        state.sent_files.pop_front();
    }
}

fn apply_event(state: &mut Stored, event: &Value) {
    let Some(seq) = event["durable"]["seq"].as_u64() else {
        return;
    };
    if seq <= state.cursor {
        return;
    }
    state.cursor = seq;
    match event["type"].as_str() {
        Some(
            "tool.activity"
            | "session.next.tool.input.started"
            | "session.next.tool.called"
            | "session.next.tool.success"
            | "session.next.tool.failed"
            | "session.next.text.started"
            | "session.next.reasoning.started",
        ) => state.run_has_activity = true,
        Some("assistant.completed" | "session.next.step.ended" | "session.next.step.failed") => {
            if let Some(id) = event["data"]["assistantMessageID"].as_str() {
                if !state.completed.iter().any(|item| item.id == id) {
                    state.completed.push_back(Candidate { id: id.to_owned() });
                }
            }
        }
        _ => {}
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn can_retry(state: &Stored, message: &Value) -> bool {
    state.retry_eligible_run
        && !state.run_has_activity
        && state.retry_attempts < 2
        && message["error"]["message"] == "HTTP transport failed"
        && message["content"].as_array().is_some_and(Vec::is_empty)
}

pub(crate) fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let parent = path.parent().context("Missing state parent")?;
    std::fs::create_dir_all(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut temp, value)?;
    temp.write_all(b"\n")?;
    temp.as_file().sync_all()?;
    temp.persist(path)
        .map_err(|e| e.error)
        .context("Cannot atomically save group state")?;
    Ok(())
}

fn prepare_directory(root: &Path, group: i64, _config: &Config) -> Result<PathBuf> {
    ensure!(group > 0, "Invalid group ID");
    std::fs::create_dir_all(root)?;
    let root = std::fs::canonicalize(root)?;
    // Windows verbatim paths are not understood consistently by Codex.
    let root = PathBuf::from(
        root.to_string_lossy()
            .strip_prefix(r"\\?\")
            .unwrap_or(&root.to_string_lossy()),
    );
    let directory = root.join(group.to_string());
    std::fs::create_dir_all(&directory)?;
    std::fs::create_dir_all(directory.join("memes"))?;
    let faces_path = directory.join("faces.csv");
    if !faces_path.exists() {
        std::fs::copy("config/faces.csv", &faces_path)
            .context("Cannot copy QQ face reference into group workspace")?;
    }
    std::fs::create_dir_all(directory.join(".alivebot"))?;
    Ok(directory)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_only_empty_transport_failures_with_bounded_budget() {
        let message = json!({"error":{"message":"HTTP transport failed"},"content":[]});
        let mut state = Stored {
            retry_eligible_run: true,
            ..Stored::default()
        };
        assert!(can_retry(&state, &message));
        state.retry_attempts = 2;
        assert!(!can_retry(&state, &message));
        state.retry_attempts = 0;
        let quota =
            json!({"error":{"message":"Provider request failed with HTTP 429"},"content":[]});
        assert!(!can_retry(&state, &quota));
        let partial = json!({"error":{"message":"HTTP transport failed"},"content":[{"type":"text","text":"partial"}]});
        assert!(!can_retry(&state, &partial));
        apply_event(
            &mut state,
            &json!({"type":"session.next.tool.called","durable":{"seq":1}}),
        );
        assert!(!can_retry(&state, &message));
        assert!(!can_retry(&Stored::default(), &message));
    }

    struct Capture(Arc<Mutex<Vec<String>>>);
    #[nagisa::async_trait]
    impl nagisa::adapter::ActionInvoker for Capture {
        fn protocol(&self) -> Protocol {
            Protocol::OneBot11
        }
        async fn send(&self, peer: &Peer, segments: &[Segment]) -> nagisa::Result<MessageId> {
            let mut captured = self.0.lock().await;
            captured.push(
                segments
                    .iter()
                    .filter_map(|s| s.as_text())
                    .collect::<Vec<_>>()
                    .join(""),
            );
            Ok(MessageId {
                peer: *peer,
                seq: captured.len() as i64,
                onebot_id: Some(captured.len() as i32),
            })
        }
        async fn call_raw(&self, _: &str, _: Value) -> nagisa::Result<Value> {
            Ok(Value::Null)
        }
        async fn get_forward_messages(&self, _: &str) -> nagisa::Result<Vec<ForwardNode>> {
            Ok(Vec::new())
        }
    }
    impl OneBotActions for Capture {}
    impl MilkyActions for Capture {}

    #[tokio::test]
    async fn empty_forward_is_reported_as_lookup_failure() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let bot = Bot::new(Arc::new(Capture(captured.clone())), Uin(1));
        assert!(
            events::forward_prompt(bot, Peer::group(Uin(123)), "empty-test")
                .await
                .is_err()
        );
        assert!(captured.lock().await.is_empty());
    }

    #[test]
    fn durable_cursor_preserves_completed_responses_across_new_steps() {
        let mut state = Stored::default();
        let ended = json!({"type":"session.next.step.ended","durable":{"seq":10},"data":{"assistantMessageID":"msg_a"}});
        apply_event(&mut state, &ended);
        assert_eq!(state.completed.front().unwrap().id, "msg_a");
        apply_event(
            &mut state,
            &json!({"type":"session.next.step.started","durable":{"seq":11}}),
        );
        apply_event(&mut state, &ended);
        assert_eq!(state.completed.len(), 1);
        assert_eq!(state.cursor, 11);
        apply_event(
            &mut state,
            &json!({"type":"session.next.step.ended","durable":{"seq":12},"data":{"assistantMessageID":"msg_b"}}),
        );
        let restored: Stored =
            serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        assert_eq!(
            restored
                .completed
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            vec!["msg_a", "msg_b"]
        );
    }

    #[tokio::test]
    async fn completed_responses_deliver_in_order_with_noop_and_no_replay() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let captured = Arc::new(Mutex::new(Vec::new()));
        let group = Group {
            ingress: Mutex::new(()),
            bot: Bot::new(Arc::new(Capture(captured.clone())), Uin(1)),
            peer: Peer::group(Uin(54321)),
            directory: temp.path().to_owned(),
            state: Mutex::new(Stored::default()),
            notify: Notify::new(),
        };
        let mut state = Stored::default();
        for (seq, id) in [(1, "msg_a"), (3, "msg_b"), (5, "msg_c")] {
            apply_event(
                &mut state,
                &json!({"type":"session.next.step.ended","durable":{"seq":seq},"data":{"assistantMessageID":id}}),
            );
            // A later generation is running; completed replies must remain queued.
            apply_event(
                &mut state,
                &json!({"type":"session.next.step.started","durable":{"seq":seq+1}}),
            );
        }
        for (index, text) in ["first reply", "<none>", "last reply"]
            .into_iter()
            .enumerate()
        {
            let candidate = state.completed.front().cloned().unwrap();
            let message =
                json!({"type":"assistant","finish":"stop","content":[{"type":"text","text":text}]});
            group
                .deliver_completed(&mut state, &candidate, &message, false)
                .await?;
            // Reload exactly as a restart would, preserving the remaining replies.
            state = serde_json::from_slice(&std::fs::read(
                temp.path().join(".alivebot/session.json"),
            )?)?;
            assert_eq!(state.completed.len(), 2 - index);
            assert!(state.sending.is_none());
        }
        assert_eq!(*captured.lock().await, vec!["first reply", "last reply"]);
        assert_eq!(state.sent_messages.len(), 2);
        apply_event(
            &mut state,
            &json!({"type":"session.next.step.ended","durable":{"seq":5},"data":{"assistantMessageID":"msg_c"}}),
        );
        assert!(state.completed.is_empty());
        Ok(())
    }

    struct EarlyEcho {
        started: Arc<Notify>,
        finish: Arc<Notify>,
    }
    #[nagisa::async_trait]
    impl nagisa::adapter::ActionInvoker for EarlyEcho {
        fn protocol(&self) -> Protocol {
            Protocol::OneBot11
        }
        async fn send(&self, peer: &Peer, _: &[Segment]) -> nagisa::Result<MessageId> {
            self.started.notify_one();
            self.finish.notified().await;
            Ok(MessageId {
                peer: *peer,
                seq: 0,
                onebot_id: Some(-456),
            })
        }
        async fn call_raw(&self, _: &str, _: Value) -> nagisa::Result<Value> {
            Ok(Value::Null)
        }
    }
    impl OneBotActions for EarlyEcho {}
    impl MilkyActions for EarlyEcho {}

    #[tokio::test]
    async fn model_echo_waits_for_send_result_and_survives_restart() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let started = Arc::new(Notify::new());
        let finish = Arc::new(Notify::new());
        let group = Arc::new(Group {
            ingress: Mutex::new(()),
            bot: Bot::new(
                Arc::new(EarlyEcho {
                    started: started.clone(),
                    finish: finish.clone(),
                }),
                Uin(1),
            ),
            peer: Peer::group(Uin(54321)),
            directory: temp.path().to_owned(),
            state: Mutex::new(Stored::default()),
            notify: Notify::new(),
        });
        let sender = group.clone();
        let sending = tokio::spawn(async move {
            let mut state = sender.state.lock().await;
            let candidate = Candidate {
                id: "msg_reply".into(),
            };
            state.completed.push_back(candidate.clone());
            sender.deliver_completed(&mut state, &candidate,
                &json!({"type":"assistant","finish":"stop","content":[{"type":"text","text":"reply"}]}), false).await
        });
        started.notified().await;
        let receiver = group.clone();
        let echo = tokio::spawn(async move { receiver.is_model_echo(-456, Uin(1)).await });
        tokio::task::yield_now().await;
        assert!(!echo.is_finished(), "report must wait for the sending lock");
        finish.notify_one();
        sending.await??;
        assert!(echo.await?);
        let saved: Stored =
            serde_json::from_slice(&std::fs::read(temp.path().join(".alivebot/session.json"))?)?;
        *group.state.lock().await = saved;
        assert!(group.is_model_echo(-456, Uin(1)).await);
        assert!(
            group.is_model_echo(-456, Uin(1)).await,
            "duplicate reports stay filtered"
        );
        assert!(
            !group.is_model_echo(-457, Uin(1)).await,
            "manual message from same account must pass"
        );
        assert!(
            !group.is_model_echo(-456, Uin(2)).await,
            "another sender must pass"
        );
        group
            .enqueue(
                Prompt::text("manual message"),
                true,
                false,
                Some("message:-457".into()),
            )
            .await?;
        assert!(group.state.lock().await.pending.back().unwrap().resume);
        Ok(())
    }

    #[test]
    fn model_echo_records_are_bounded_and_old_state_remains_readable() {
        let mut state: Stored = serde_json::from_str(r#"{"session_id":"ses_old"}"#).unwrap();
        assert!(state.sent_messages.is_empty());
        for id in 1..=(SENT_MESSAGE_LIMIT as i32 + 1) {
            remember_model_message(&mut state, id, Uin(1));
        }
        assert_eq!(state.sent_messages.len(), SENT_MESSAGE_LIMIT);
        assert_eq!(state.sent_messages.front().unwrap().id, 2);
        remember_model_message(&mut state, SENT_MESSAGE_LIMIT as i32 + 1, Uin(1));
        assert_eq!(state.sent_messages.len(), SENT_MESSAGE_LIMIT);
    }
    #[tokio::test]
    async fn failed_get_returns_context_without_sending_a_group_message() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let captured = Arc::new(Mutex::new(Vec::new()));
        // Capture does not implement get_message, simulating an unavailable lookup.
        let group = Group {
            ingress: Mutex::new(()),
            bot: Bot::new(Arc::new(Capture(captured.clone())), Uin(1)),
            peer: Peer::group(Uin(54321)),
            directory: temp.path().to_owned(),
            state: Mutex::new(Stored::default()),
            notify: Notify::new(),
        };
        let mut state = Stored::default();
        let candidate = Candidate {
            id: "msg_get".into(),
        };
        state.completed.push_back(candidate.clone());
        let message = json!({"type":"assistant","finish":"stop","content":[{"type":"text","text":"<get id:-123>"}]});
        group
            .deliver_completed(&mut state, &candidate, &message, false)
            .await?;
        assert!(captured.lock().await.is_empty());
        assert!(state.completed.is_empty());
        assert!(state.sending.is_none());
        assert_eq!(state.pending.len(), 1);
        let input = state.pending.front().unwrap();
        assert!(input.resume && !input.own);
        assert!(
            input
                .prompt
                .text
                .contains("<get id:-123 error:unavailable>")
        );
        Ok(())
    }

    struct ImageCapture(Arc<Mutex<Vec<PathBuf>>>);
    #[nagisa::async_trait]
    impl nagisa::adapter::ActionInvoker for ImageCapture {
        fn protocol(&self) -> Protocol {
            Protocol::OneBot11
        }
        async fn call_raw(&self, _: &str, _: Value) -> nagisa::Result<Value> {
            Ok(Value::Null)
        }
        async fn send(&self, peer: &Peer, parts: &[Segment]) -> nagisa::Result<MessageId> {
            let mut images = self.0.lock().await;
            for part in parts {
                if let Segment::Image { res, .. } = part {
                    if let Some(ResourceSource::Path(path)) = &res.source {
                        images.push(path.clone());
                    }
                }
            }
            Ok(MessageId {
                peer: *peer,
                seq: 0,
                onebot_id: Some(321),
            })
        }
    }
    impl OneBotActions for ImageCapture {}
    impl MilkyActions for ImageCapture {}

    #[tokio::test]
    async fn image_delivery_uses_each_groups_directory_and_records_its_echo_id() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let captured = Arc::new(Mutex::new(Vec::new()));
        for group_id in [123, 456] {
            let directory = prepare_directory(temp.path(), group_id, &Config::default())?;
            std::fs::write(
                directory.join("memes/image.png"),
                format!("group {group_id}"),
            )?;
            let group = Group {
                ingress: Mutex::new(()),
                bot: Bot::new(Arc::new(ImageCapture(captured.clone())), Uin(1)),
                peer: Peer::group(Uin(group_id)),
                directory,
                state: Mutex::new(Stored::default()),
                notify: Notify::new(),
            };
            let mut state = Stored::default();
            let candidate = Candidate {
                id: format!("msg_image_{group_id}"),
            };
            state.completed.push_back(candidate.clone());
            group.deliver_completed(&mut state,&candidate,
                &json!({"type":"assistant","finish":"stop","content":[{"type":"text","text":"<img:memes/image.png>"}]}),false).await?;
            assert_eq!(state.sent_messages.back().unwrap().id, 321);
            assert!(state.pending.is_empty());
        }
        let paths = captured.lock().await;
        assert_eq!(paths.len(), 2);
        assert_ne!(paths[0], paths[1]);
        assert_eq!(std::fs::read(&paths[0])?, b"group 123");
        assert_eq!(std::fs::read(&paths[1])?, b"group 456");
        Ok(())
    }

    struct FileCapture {
        uploads: Arc<Mutex<Vec<(Uin, PathBuf, String)>>>,
        id: String,
        fail: bool,
    }
    #[nagisa::async_trait]
    impl nagisa::adapter::ActionInvoker for FileCapture {
        fn protocol(&self) -> Protocol {
            Protocol::OneBot11
        }
        async fn call_raw(&self, _: &str, _: Value) -> nagisa::Result<Value> {
            Ok(Value::Null)
        }
        async fn send(&self, _: &Peer, _: &[Segment]) -> nagisa::Result<MessageId> {
            panic!("files must use the upload API, not message segments")
        }
        async fn upload_group_file(
            &self,
            group: Uin,
            source: ResourceSource,
            name: &str,
            folder: Option<&str>,
        ) -> nagisa::Result<String> {
            assert!(folder.is_none());
            let ResourceSource::Path(path) = source else {
                panic!("expected local file")
            };
            self.uploads.lock().await.push((group, path, name.into()));
            if self.fail {
                Err(nagisa::Error::action("mock upload failed"))
            } else {
                Ok(self.id.clone())
            }
        }
    }
    impl OneBotActions for FileCapture {}
    impl MilkyActions for FileCapture {}

    #[tokio::test]
    async fn file_upload_uses_a_separate_api_and_persists_echo_id_without_receipt() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let directory = prepare_directory(temp.path(), 123, &Config::default())?;
        std::fs::write(directory.join("报告 1.zip"), "fixture")?;
        let uploads = Arc::new(Mutex::new(Vec::new()));
        let group = Group {
            ingress: Mutex::new(()),
            bot: Bot::new(
                Arc::new(FileCapture {
                    uploads: uploads.clone(),
                    id: "file-123".into(),
                    fail: false,
                }),
                Uin(1),
            ),
            peer: Peer::group(Uin(123)),
            directory: directory.clone(),
            state: Mutex::new(Stored::default()),
            notify: Notify::new(),
        };
        let mut state = Stored::default();
        let candidate = Candidate {
            id: "msg_file".into(),
        };
        state.completed.push_back(candidate.clone());
        group.deliver_completed(&mut state,&candidate,
            &json!({"type":"assistant","finish":"stop","content":[{"type":"text","text":"<file path:报告 1.zip>"}]}),false).await?;
        assert!(state.sent_messages.is_empty());
        assert_eq!(state.sent_files.len(), 1);
        assert!(state.pending.is_empty());
        let uploaded = uploads.lock().await;
        assert_eq!(uploaded.len(), 1);
        assert_eq!(uploaded[0].0, Uin(123));
        assert_eq!(uploaded[0].2, "报告 1.zip");
        assert_eq!(std::fs::read(&uploaded[0].1)?, b"fixture");
        drop(uploaded);
        let restored: Stored =
            serde_json::from_slice(&std::fs::read(directory.join(".alivebot/session.json"))?)?;
        *group.state.lock().await = restored;
        assert!(group.is_model_file_echo("file-123", Uin(1)).await);
        assert!(!group.is_model_file_echo("manual-file", Uin(1)).await);
        assert!(!group.is_model_file_echo("file-123", Uin(2)).await);
        assert!(!group.is_model_file_echo("", Uin(1)).await);
        Ok(())
    }

    #[tokio::test]
    async fn failed_file_upload_has_no_success_receipt_and_is_not_retried() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let directory = prepare_directory(temp.path(), 123, &Config::default())?;
        std::fs::write(directory.join("report.txt"), "fixture")?;
        let uploads = Arc::new(Mutex::new(Vec::new()));
        let group = Group {
            ingress: Mutex::new(()),
            bot: Bot::new(
                Arc::new(FileCapture {
                    uploads: uploads.clone(),
                    id: String::new(),
                    fail: true,
                }),
                Uin(1),
            ),
            peer: Peer::group(Uin(123)),
            directory,
            state: Mutex::new(Stored::default()),
            notify: Notify::new(),
        };
        let mut state = Stored::default();
        let candidate = Candidate {
            id: "msg_failed_file".into(),
        };
        state.completed.push_back(candidate.clone());
        group.deliver_completed(&mut state,&candidate,
            &json!({"type":"assistant","finish":"stop","content":[{"type":"text","text":"<file path:report.txt>"}]}),false).await?;
        assert!(state.sent_files.is_empty());
        assert!(state.sending.is_none());
        assert_eq!(uploads.lock().await.len(), 1);
        assert!(state.pending.is_empty());
        Ok(())
    }

    #[test]
    fn file_echo_records_are_bounded_and_older_state_remains_readable() {
        let mut state: Stored = serde_json::from_str(r#"{"session_id":"old"}"#).unwrap();
        assert!(state.sent_files.is_empty());
        for i in 0..=SENT_MESSAGE_LIMIT {
            remember_model_file(&mut state, format!("file-{i}"), Uin(1));
        }
        assert_eq!(state.sent_files.len(), SENT_MESSAGE_LIMIT);
        assert_eq!(state.sent_files.front().unwrap().id, "file-1");
    }

    #[tokio::test]
    async fn successful_upload_without_a_file_id_does_not_invent_an_echo_record() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let directory = prepare_directory(temp.path(), 123, &Config::default())?;
        std::fs::write(directory.join("report.txt"), "fixture")?;
        let uploads = Arc::new(Mutex::new(Vec::new()));
        let group = Group {
            ingress: Mutex::new(()),
            bot: Bot::new(
                Arc::new(FileCapture {
                    uploads,
                    id: String::new(),
                    fail: false,
                }),
                Uin(1),
            ),
            peer: Peer::group(Uin(123)),
            directory,
            state: Mutex::new(Stored::default()),
            notify: Notify::new(),
        };
        let mut state = Stored::default();
        let candidate = Candidate {
            id: "msg_file_no_id".into(),
        };
        state.completed.push_back(candidate.clone());
        group.deliver_completed(&mut state,&candidate,
            &json!({"type":"assistant","finish":"stop","content":[{"type":"text","text":"<file path:report.txt>"}]}),false).await?;
        assert!(state.sent_files.is_empty());
        assert!(state.pending.is_empty());
        Ok(())
    }

    #[test]
    fn group_config_and_session_survive_atomic_updates() {
        let temp = tempfile::tempdir().unwrap();
        let config = Config::default();
        let dir = prepare_directory(temp.path(), 12345, &config).unwrap();
        assert!(dir.join("memes").is_dir());
        std::fs::write(dir.join("memes/keep.png"), "fixture").unwrap();
        prepare_directory(temp.path(), 12345, &config).unwrap();
        assert_eq!(
            std::fs::read(dir.join("memes/keep.png")).unwrap(),
            b"fixture"
        );
        std::fs::write(dir.join("keep.txt"), "keep").unwrap();
        let path = dir.join(".alivebot/session.json");
        let mut saved = Stored {
            session_id: "ses_first".into(),
            cursor: 42,
            ..Stored::default()
        };
        write_json(&path, &saved).unwrap();
        saved.session_id = "ses_second".into();
        write_json(&path, &saved).unwrap();
        let loaded: Stored = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(loaded.session_id, "ses_second");
        assert_eq!(loaded.cursor, 42);
        assert!(dir.join("keep.txt").exists());
        assert!(dir.join(".alivebot").is_dir());
        assert!(!dir.join(".opencode/opencode.json").exists());
    }
}
