use crate::utilities;
use nagisa::prelude::*;

#[command("/ping")]
async fn ping(reply: Reply) -> HandlerResult {
    reply.text("pong").await?;
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
        if let Some(emoji) = utilities::napcat_emoji_id(emoji) {
            bot.actions()
                .set_msg_reaction(&id, emoji.as_str(), true)
                .await?;
        }
    }
    Ok(())
}
