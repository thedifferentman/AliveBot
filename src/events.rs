use crate::actions::send_message;
use crate::context_manage::{CONTEXT, Context};
use crate::llm_calling::request_llamacpp;
use crate::tools::emoji::reaction_name;
use crate::tools::message::{LiteSegment, segments_to_lite};
use crate::tools::static_map::ID_MAP;
use nagisa::*;
use serde_json::json;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::warn;

fn rule_not_command(ctx: &Ctx) -> bool {
    let Some(message) = ctx.message() else {
        return true;
    };
    let Some(Segment::Text(text)) = message.content.first() else {
        return true;
    };
    !text.trim_start().starts_with('/')
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
    let Some(message_id) = message.id.onebot_id else {
        warn!("message without a OneBot message_id was skipped");
        return Ok(());
    };
    let shared_context = {
        let mut guard = CONTEXT.get().unwrap().lock().await;
        Arc::clone(
            guard
                .entry(message.peer.clone())
                .or_insert_with(|| Arc::new(Mutex::new(Context::new()))),
        )
    };
    let mut context = shared_context.lock().await;
    context
        .push(json!({
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
        }))
        .await;
    let content = segments_to_lite(message.content)
        .await
        .into_iter()
        .map(LiteSegment::into)
        .collect();
    context.extend(content).await;
    context
        .push(json!({
            "type":"text",
            "text":"</message>"
        }))
        .await;
    context.cut().await.unwrap();
    let answer = request_llamacpp(&*context)
        .await
        .map_err(|e| Error::action(e.to_string()))?;
    drop(context);
    send_message(bot, answer.as_str(), message.peer).await
}

pub async fn forward_message(bot: Bot, forward_id: &str, peer: Peer) -> HandlerResult {
    let forward = bot.get_forward_messages(forward_id).await?;
    let shared_context = {
        let mut guard = CONTEXT.get().unwrap().lock().await;
        Arc::clone(
            guard
                .entry(peer.clone())
                .or_insert_with(|| Arc::new(Mutex::new(Context::new()))),
        )
    };
    let mut context = shared_context.lock().await;
    context
        .push(json!({
            "type":"text",
            "text":"<forward>"
        }))
        .await;
    for node in forward {
        context
            .push(json!({
                "type":"text",
                "text":format!("<message id:0, sender:{}>",
                    ID_MAP.get_by_left(
                        node.user.0.to_string().as_str()
                    )
                    .unwrap_or(node.name.as_str()))
            }))
            .await;
        let content = segments_to_lite(node.content)
            .await
            .into_iter()
            .map(LiteSegment::into)
            .collect();
        context.extend(content).await;
        context
            .push(json!({
                "type":"text",
                "text":"</message>"
            }))
            .await;
    }
    context
        .push(json!({
            "type":"text",
            "text":"</forward>"
        }))
        .await;
    context.cut().await.unwrap();
    let answer = request_llamacpp(&*context)
        .await
        .map_err(|e| Error::action(e.to_string()))?;
    drop(context);
    send_message(bot, answer.as_str(), peer).await
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
                .or_insert_with(|| Arc::new(Mutex::new(Context::new()))),
        )
    };
    let mut context = shared_context.lock().await;
    context
        .push(json!({
            "type":"text",
            "text":format!(
                "<emoji_like sender:{sender}, messageid:{message_id}, face:{face}>"
            )
        }))
        .await;
    context.cut().await.unwrap();
    let answer = request_llamacpp(&*context)
        .await
        .map_err(|e| Error::action(e.to_string()))?;
    drop(context);
    send_message(bot, answer.as_str(), peer).await
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
    let sender = group_member_name(&bot, group, sender).await?;
    let receiver = group_member_name(&bot, group, receiver).await?;
    let shared_context = {
        let mut guard = CONTEXT.get().unwrap().lock().await;
        Arc::clone(
            guard
                .entry(peer)
                .or_insert_with(|| Arc::new(Mutex::new(Context::new()))),
        )
    };
    let mut context = shared_context.lock().await;
    context
        .push(json!({
            "type":"text",
            "text":format!("<nudge sender:{sender}, receiver:{receiver}>")
        }))
        .await;
    context.cut().await.unwrap();
    let answer = request_llamacpp(&*context)
        .await
        .map_err(|e| Error::action(e.to_string()))?;
    drop(context);
    send_message(bot, answer.as_str(), peer).await
}
