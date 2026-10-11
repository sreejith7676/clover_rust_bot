use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;
use teloxide::types::UserId;
use tokio::sync::RwLock;
use crate::helpers::logs::{ListenerErrorHandler, LogBuffer};
use crate::models::DeviceSummary;

pub struct AppState {
    pub bot_start_time: Instant,
    pub stats: RwLock<BotStats>,
    pub actual_filenames: RwLock<HashMap<String, String>>,
    pub http_client: reqwest::Client,
    pub device_statuses: RwLock<HashMap<String, String>>,
    pub devices_cache: RwLock<Vec<DeviceSummary>>,
    pub banner_file_id: RwLock<Option<String>>,
    pub log_buffer: LogBuffer,
    pub listener_error_handler: Arc<ListenerErrorHandler>,
}

pub struct BotStats {
    pub total_users: HashSet<UserId>,
    pub commands_processed: u64,
    pub devices_queried: u64,
}

impl AppState {
    pub fn new(
        initial_statuses: HashMap<String, String>,
        log_buffer: LogBuffer,
        listener_error_handler: Arc<ListenerErrorHandler>,
    ) -> Self {
        Self {
            bot_start_time: Instant::now(),
            stats: RwLock::new(BotStats {
                total_users: HashSet::new(),
                commands_processed: 0,
                devices_queried: 0,
            }),
            actual_filenames: RwLock::new(HashMap::new()),
            http_client: reqwest::Client::builder()
                .pool_idle_timeout(std::time::Duration::from_secs(90))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            device_statuses: RwLock::new(initial_statuses),
            devices_cache: RwLock::new(Vec::new()),
            banner_file_id: RwLock::new(None),
            log_buffer,
            listener_error_handler,
        }
    }
}
