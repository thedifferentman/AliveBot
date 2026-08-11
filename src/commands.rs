use crate::tools;
use nagisa::prelude::*;
use tracing::{error, info, warn};

#[command("/ping")]
async fn ping(reply: Reply) -> HandlerResult {
    reply.text("pong").await?;
    Ok(())
}

#[command("/face")]
async fn face(reply: Reply, CommandArg(segments): CommandArg) -> HandlerResult {
    let Some(segment) = segments.first() else {
        bail!(
            ActionErrorKind::BadParams,
            "Need 1 number param, find no param."
        );
    };
    let Some(text) = segment.as_text() else {
        bail!(
            ActionErrorKind::BadParams,
            "Need 1 number param, find other type."
        );
    };
    reply.face(text).await?;
    Ok(())
}

#[command("/faceid")]
async fn faceid(reply: Reply, CommandArg(segments): CommandArg) -> HandlerResult {
    let Some(segment) = segments.first() else {
        bail!(
            ActionErrorKind::BadParams,
            "Need 1 face param, find no param."
        );
    };
    let Segment::Face { id, .. } = segment else {
        bail!(
            ActionErrorKind::BadParams,
            "Need 1 face param, find other type."
        );
    };
    reply.text(id).await?;
    Ok(())
}

#[derive(Args)]
struct ReactArgs {
    #[arg(reply)]
    id: MessageId,

    #[arg(face, rest)]
    faces: Vec<String>,

    #[arg(rest)]
    content: String,
}

#[command("/react")]
async fn react(bot: Bot, Args(ReactArgs { id, faces, content }): Args<ReactArgs>) -> HandlerResult {
    for face in faces {
        bot.actions()
            .set_msg_reaction(&id, face.as_str(), true)
            .await?;
    }
    let emojis = emojito::find_emoji(content);
    for emoji in emojis {
        if let Some(emoji) = tools::emoji::napcat_emoji_id(emoji) {
            bot.actions()
                .set_msg_reaction(&id, emoji.as_str(), true)
                .await?;
        } else {
            warn!("Do not support hybrid emoji \"{}\".", emoji.glyph);
        }
    }
    Ok(())
}
