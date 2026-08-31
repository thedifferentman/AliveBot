use nagisa::prelude::*;
use std::collections::HashSet;
use std::sync::Arc;

pub struct GroupWhitelist {
    groups: HashSet<Uin>,
}

impl GroupWhitelist {
    pub fn new(groups: impl IntoIterator<Item = i64>) -> Self {
        Self {
            groups: groups.into_iter().map(Uin).collect(),
        }
    }
}

#[nagisa::async_trait]
impl Middleware for GroupWhitelist {
    async fn handle(&self, ctx: Arc<Ctx>, next: Next<'_>) -> Flow {
        match ctx.event().group() {
            Some(group) if self.groups.contains(&group) => next.run(ctx).await,
            _ => Flow::Stop,
        }
    }
}
