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
        println!("[{}][task::save_snapshot] No messages, skipping", route.name);
        return Ok(());
    }

    let last_saved_count = task_store.lock().unwrap().get_last_saved_count().unwrap_or(0);

    if current_count <= last_saved_count {
        println!("[{}][task::save_snapshot] Nothing new to save (count={})", route.name, current_count);
        return Ok(());
    }

    println!("[{}][task::save_snapshot] Saving snapshot (count {} -> {})", route.name, last_saved_count, current_count);
    send_tx(inbox.saveSnapshot().send().await, "saveSnapshot", route.name).await
}
