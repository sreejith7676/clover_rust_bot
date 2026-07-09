use std::sync::Arc;
use teloxide::types::UserId;
use crate::state::AppState;

pub async fn track_user(user_id: UserId, state: &Arc<AppState>) {
    let mut stats = state.stats.write().await;
    stats.commands_processed += 1;
    stats.total_users.insert(user_id);
}
