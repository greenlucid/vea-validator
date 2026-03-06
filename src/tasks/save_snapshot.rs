use tracing::info;
use crate::config::Route;
use crate::contracts::IVeaInbox;
use crate::tasks::{send_tx, TaskStore};
use std::sync::{Arc, Mutex};

pub async fn execute(
    route: &Route,
    task_store: &Arc<Mutex<TaskStore>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let inbox = IVeaInbox::new(route.inbox_address, route.inbox_provider.clone());
    let current_count = inbox.count().call().await?;

    if current_count == 0 {
        info!(logger = "SaveSnapshot", route = route.name, "No messages, skipping");
        return Ok(());
    }

    let last_saved_count = task_store.lock().unwrap().get_last_saved_count().unwrap_or(0);

    if current_count <= last_saved_count {
        info!(logger = "SaveSnapshot", route = route.name, count = current_count, "Nothing new to save");
        return Ok(());
    }

    info!(logger = "SaveSnapshot", route = route.name, from = last_saved_count, to = current_count, "Saving snapshot");
    send_tx(inbox.saveSnapshot().send().await, "saveSnapshot", route.name).await
}
