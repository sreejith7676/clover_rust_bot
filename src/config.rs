use teloxide::types::{ChatId, UserId};

// User and Group IDs
pub const ADMIN_IDS: [u64; 1] = [1432942752];
pub const ALLOWED_GROUP_IDS: [i64; 2] = [-1002364464128, -1002485722404];
pub const SPECIAL_GROUP: ChatId = ChatId(-1002485722404);

// Links
pub const GROUP_LINK: &str = "https://t.me/TheCloverProject_Chat";

// --- GITHUB CONFIGURATION ---
pub const GITHUB_ORG: &str = "The-Clover-Project";

// Unified repository (TheCloverProject.com is archived and merged into official_updates)
pub const REPO_UPDATES: &str = "official_updates";

// Supported branch hierarchy: A17 (default), A16, A15
pub const DEFAULT_BRANCH: &str = "17";
pub const BRANCH_A17: &str = "17";
pub const BRANCH_A16: &str = "16";
pub const BRANCH_A15: &str = "15";
pub const ALL_BRANCHES: [&str; 3] = [BRANCH_A17, BRANCH_A16, BRANCH_A15];

// Directories & paths within official_updates
pub const SITE_DEVICES_JSON: &str = "site/devices.json";
pub const DIR_SITE_DEVICES: &str = "site/devices";
pub const DIR_CHANGELOGS: &str = "changelogs";
#[allow(dead_code)]
pub const DIR_DEVICES: &str = "devices"; 

// Local files
pub const LOG_FILE: &str = "clover_bot.log";
pub const STATUS_FILE: &str = "device_status.json"; 

// Helper functions
pub fn is_admin(user_id: UserId) -> bool {
    ADMIN_IDS.contains(&user_id.0)
}

pub fn is_allowed(chat_id: ChatId) -> bool {
    ALLOWED_GROUP_IDS.contains(&chat_id.0)
}
