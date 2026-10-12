use std::collections::{HashMap, HashSet};
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use teloxide::types::{FileId, UserId};
use tokio::sync::{Notify, RwLock};

use crate::config::Config;
use crate::helpers::logs::LogBuffer;
use crate::models::{DeviceSummary, ResolvedUpdates};

/// How long a resolved (or confirmed-missing) update lookup is reused.
pub const UPDATE_CACHE_TTL: Duration = Duration::from_secs(300);

pub struct AppState {
    pub config: Config,
    pub bot_start_time: Instant,
    pub stats: RwLock<BotStats>,
    pub actual_filenames: RwLock<HashMap<String, String>>,
    pub http_client: reqwest::Client,
    pub device_statuses: RwLock<HashMap<String, String>>,
    pub devices_cache: RwLock<Vec<DeviceSummary>>,
    /// lowercase codename -> (fetched at, result). `None` = confirmed no update entry.
    pub update_cache: RwLock<HashMap<String, (Instant, Option<ResolvedUpdates>)>>,
    pub banner_file_id: RwLock<Option<FileId>>,
    pub log_buffer: LogBuffer,
    cooldowns: Mutex<HashMap<UserId, Instant>>,
    /// Notified to request a graceful shutdown (used by /reboot and SIGTERM).
    pub shutdown: Notify,
    pub reboot_requested: AtomicBool,
}

pub struct BotStats {
    pub total_users: HashSet<UserId>,
    pub commands_processed: u64,
    pub devices_queried: u64,
}

pub fn build_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent("Clover-Bot-Rust")
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .pool_idle_timeout(Duration::from_secs(90))
        .build()
        .expect("failed to build HTTP client (TLS backend unavailable?)")
}

impl AppState {
    pub fn new(
        config: Config,
        initial_statuses: HashMap<String, String>,
        log_buffer: LogBuffer,
    ) -> Self {
        Self {
            config,
            bot_start_time: Instant::now(),
            stats: RwLock::new(BotStats {
                total_users: HashSet::new(),
                commands_processed: 0,
                devices_queried: 0,
            }),
            actual_filenames: RwLock::new(HashMap::new()),
            http_client: build_http_client(),
            device_statuses: RwLock::new(initial_statuses),
            devices_cache: RwLock::new(Vec::new()),
            update_cache: RwLock::new(HashMap::new()),
            banner_file_id: RwLock::new(None),
            log_buffer,
            cooldowns: Mutex::new(HashMap::new()),
            shutdown: Notify::new(),
            reboot_requested: AtomicBool::new(false),
        }
    }

    /// Returns `true` (and records the attempt) if `user` may run another
    /// network-heavy command now, `false` if they are still in the cooldown window.
    pub fn check_cooldown(&self, user: UserId, window: Duration) -> bool {
        let mut map = self.cooldowns.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        if map.len() > 1000 {
            map.retain(|_, last| now.duration_since(*last) < window);
        }
        match map.get(&user) {
            Some(last) if now.duration_since(*last) < window => false,
            _ => {
                map.insert(user, now);
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cooldown_blocks_rapid_repeats_per_user() {
        let state = AppState::new(Config::default(), HashMap::new(), LogBuffer::new(10));
        let w = Duration::from_secs(60);
        assert!(state.check_cooldown(UserId(1), w));
        assert!(!state.check_cooldown(UserId(1), w));
        assert!(state.check_cooldown(UserId(2), w)); // independent per user
        assert!(state.check_cooldown(UserId(1), Duration::ZERO)); // window elapsed
    }
}
