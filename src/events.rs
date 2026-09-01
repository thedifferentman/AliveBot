use crate::CONFIG;
use crate::actions::send_message;
use crate::context_manage::{CONTEXT, SharedContext};
use crate::llm_calling::request_llamacpp;
use crate::tools::emoji::reaction_name;
use crate::tools::message::{LiteSegment, segments_to_lite};
use crate::tools::static_map::ID_MAP;
use nagisa::*;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tracing::{info, warn};

fn rule_not_command(ctx: &Ctx) -> bool {
    let Some(message) = ctx.message() else {
        return true;
    };
    let Some(Segment::Text(text)) = message.content.first() else {
        return true;
    };
    !text.trim_start().starts_with('/')
}

fn is_own_account(bot: &Bot, user: Uin) -> bool {
    user == bot.self_id() || CONFIG.get().unwrap().self_accounts.contains(&user.0)
}

async fn group_member_name(bot: &Bot, group: Uin, user: Uin) -> Result<String> {
    if let Some(name) = ID_MAP.get_by_left(&user.to_string()) {
        Ok(name.to_owned())
    } else {
        Ok(bot
            .get_group_member_info(group, user, false)
            .await?
            .display_name()
            .to_owned())
    }
}

#[event(Message, gate = Rule::pred(rule_not_command))]
async fn common_message(bot: Bot, message: MessageEvent) -> HandlerResult {
    let is_own_message = message.is_self || is_own_account(&bot, message.sender);
    let Some(message_id) = message.id.onebot_id else {
        warn!("message without a OneBot message_id was skipped");
        return Ok(());
    };
    let shared_context = {
        let mut guard = CONTEXT.get().unwrap().lock().await;
        Arc::clone(
            guard
                .entry(message.peer.clone())
                .or_insert_with(|| Arc::new(SharedContext::new())),
        )
    };
    let expected_revision = shared_context
        .revision
        .fetch_add(1, Ordering::Relaxed)
        .wrapping_add(1);
    let mut context = shared_context.context.lock().await;
    context.push(json!({
        "type":"text",
        "text":format!("<message id:{}, sender:{}>",
            message_id,
            ID_MAP.get_by_left(
                &message.sender.to_string()
            )
            .unwrap_or_else(
                || message.member.as_ref().unwrap().nickname.as_str()
            )
        )
    }));
    let content = segments_to_lite(message.content)
        .await
        .into_iter()
        .map(LiteSegment::into)
        .collect();
    context.extend(content);
    context.push(json!({
        "type":"text",
        "text":"</message>"
    }));
    context.cut().await.unwrap();
    if is_own_message {
        return Ok(());
    }
    let current_revision = shared_context.revision.load(Ordering::Relaxed);
    if current_revision != expected_revision {
        info!(
            expected_revision,
            current_revision, "Newer message arrived; model call was cancelled"
        );
        return Ok(());
    }
    let answer = request_llamacpp(&*context)
        .await
        .map_err(|e| Error::action(e.to_string()))?;
    drop(context);
    send_message(
        bot,
        answer.as_str(),
        message.peer,
        shared_context,
        expected_revision,
    )
    .await
}

pub async fn forward_message(
    bot: Bot,
    forward_id: &str,
    peer: Peer,
    shared_context: Arc<SharedContext>,
    expected_revision: u64,
) -> HandlerResult {
    if shared_context.revision.load(Ordering::Relaxed) != expected_revision {
        return Ok(());
    }
    let forward = bot.get_forward_messages(forward_id).await?;
    let mut context = shared_context.context.lock().await;
    context.push(json!({
        "type":"text",
        "text":"<forward>"
    }));
    for node in forward {
        context.push(json!({
            "type":"text",
            "text":format!("<message id:0, sender:{}>",
                ID_MAP.get_by_left(
                    node.user.0.to_string().as_str()
                )
                .unwrap_or(node.name.as_str()))
        }));
        let content = segments_to_lite(node.content)
            .await
            .into_iter()
            .map(LiteSegment::into)
            .collect();
        context.extend(content);
        context.push(json!({
            "type":"text",
            "text":"</message>"
        }));
    }
    context.push(json!({
        "type":"text",
        "text":"</forward>"
    }));
    context.cut().await.unwrap();
    let current_revision = shared_context.revision.load(Ordering::Relaxed);
    if current_revision != expected_revision {
        info!(
            expected_revision,
            current_revision, "Newer message arrived; model call was cancelled"
        );
        return Ok(());
    }
    let answer = request_llamacpp(&*context)
        .await
        .map_err(|e| Error::action(e.to_string()))?;
    drop(context);
    send_message(
        bot,
        answer.as_str(),
        peer,
        shared_context,
        expected_revision,
    )
    .await
}

#[event(Reaction, gate = Rule::pred(rule_not_command))]
async fn reaction_message(bot: Bot, notice: Notice) -> HandlerResult {
    let Notice::Reaction {
        group,
        user,
        seq,
        face_id,
        kind,
        is_add: true,
        ..
    } = notice
    else {
        return Ok(());
    };
    let is_own_action = is_own_account(&bot, user);
    let Some(face) = reaction_name(&face_id, kind) else {
        return Ok(());
    };
    let Ok(message_id) = i32::try_from(seq) else {
        warn!("reaction notice contains an invalid OneBot message_id: {seq}");
        return Ok(());
    };
    if message_id == 0 {
        warn!("reaction notice contains an empty OneBot message_id");
        return Ok(());
    }
    let peer = Peer::group(group);
    let sender = group_member_name(&bot, group, user).await?;
    let shared_context = {
        let mut guard = CONTEXT.get().unwrap().lock().await;
        Arc::clone(
            guard
                .entry(peer)
                .or_insert_with(|| Arc::new(SharedContext::new())),
        )
    };
    let expected_revision = if is_own_action {
        shared_context
            .revision
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1)
    } else {
        shared_context.revision.load(Ordering::Relaxed)
    };
    let mut context = shared_context.context.lock().await;
    context.push(json!({
        "type":"text",
        "text":format!(
            "<emoji_like sender:{sender}, messageid:{message_id}, face:{face}>"
        )
    }));
    context.cut().await.unwrap();
    if is_own_action {
        return Ok(());
    }
    let current_revision = shared_context.revision.load(Ordering::Relaxed);
    if current_revision != expected_revision {
        info!(
            expected_revision,
            current_revision, "Newer message arrived; model call was cancelled"
        );
        return Ok(());
    }
    let answer = request_llamacpp(&*context)
        .await
        .map_err(|e| Error::action(e.to_string()))?;
    drop(context);
    send_message(
        bot,
        answer.as_str(),
        peer,
        shared_context,
        expected_revision,
    )
    .await
}

#[event(Nudge, gate = Rule::pred(rule_not_command))]
async fn nudge_message(bot: Bot, notice: Notice) -> HandlerResult {
    let Notice::GroupNudge {
        group,
        sender,
        receiver,
        ..
    } = notice
    else {
        return Ok(());
    };
    let peer = Peer::group(group);
    let is_own_action = is_own_account(&bot, sender);
    let sender = group_member_name(&bot, group, sender).await?;
    let receiver = group_member_name(&bot, group, receiver).await?;
    let shared_context = {
        let mut guard = CONTEXT.get().unwrap().lock().await;
        Arc::clone(
            guard
                .entry(peer)
                .or_insert_with(|| Arc::new(SharedContext::new())),
        )
    };
    let expected_revision = if is_own_action {
        shared_context
            .revision
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1)
    } else {
        shared_context.revision.load(Ordering::Relaxed)
    };
    let mut context = shared_context.context.lock().await;
    context.push(json!({
        "type":"text",
        "text":format!("<nudge sender:{sender}, receiver:{receiver}>")
    }));
    context.cut().await.unwrap();
    if is_own_action {
        return Ok(());
    }
    let current_revision = shared_context.revision.load(Ordering::Relaxed);
    if current_revision != expected_revision {
        info!(
            expected_revision,
            current_revision, "Newer message arrived; model call was cancelled"
        );
        return Ok(());
    }
    let answer = request_llamacpp(&*context)
        .await
        .map_err(|e| Error::action(e.to_string()))?;
    drop(context);
    send_message(
        bot,
        answer.as_str(),
        peer,
        shared_context,
        expected_revision,
    )
    .await
}
