use std::collections::{HashMap, HashSet};
use std::time::Instant;
use teloxide::types::UserId;
use tokio::sync::RwLock;
use crate::models::DeviceSummary; // Import the new struct

pub struct AppState {
    pub bot_start_time: Instant,
    pub stats: RwLock<BotStats>,
    pub actual_filenames: RwLock<HashMap<String, String>>,
    pub http_client: reqwest::Client,
    pub device_statuses: RwLock<HashMap<String, String>>,
    // NEW: Cache for the categorized list
    pub devices_cache: RwLock<Vec<DeviceSummary>>,
}

pub struct BotStats {
    pub total_users: HashSet<UserId>,
    pub commands_processed: u64,
    pub devices_queried: u64,
}

impl AppState {
    pub fn new(initial_statuses: HashMap<String, String>) -> Self {
        Self {
            bot_start_time: Instant::now(),
            stats: RwLock::new(BotStats {
                total_users: HashSet::new(),
                commands_processed: 0,
                devices_queried: 0,
            }),
            actual_filenames: RwLock::new(HashMap::new()),
            http_client: reqwest::Client::new(),
            device_statuses: RwLock::new(initial_statuses),
            // Initialize empty cache
            devices_cache: RwLock::new(Vec::new()),
        }
    }
}
