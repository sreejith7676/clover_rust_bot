use teloxide::types::{ChatId, UserId};

// User and Group IDs
pub const ADMIN_IDS: [u64; 1] = [1432942752];
pub const ALLOWED_GROUP_IDS: [i64; 2] = [-1002364464128, -1002485722404];
pub const SPECIAL_GROUP: ChatId = ChatId(-1002485722404);

// Links
pub const GROUP_LINK: &str = "https://t.me/TheCloverProject_Chat";

// --- GITHUB CONFIGURATION ---
pub const GITHUB_ORG: &str = "The-Clover-Project";
pub const REPO_DATE: &str = "official_updates";
pub const BRANCH_DATE: &str = "16";
pub const DIR_DATE: &str = "devices";

pub const REPO_MAIN: &str = "TheCloverProject.com";
pub const BRANCH_MAIN: &str = "main";
pub const DIR_MAIN: &str = "data/devices"; 

pub const LOG_FILE: &str = "clover_bot.log";
// New constant for saving the status list
pub const STATUS_FILE: &str = "device_status.json"; 

// Helper functions
pub fn is_admin(user_id: UserId) -> bool {
    ADMIN_IDS.contains(&user_id.0)
}

pub fn is_allowed(chat_id: ChatId) -> bool {
    ALLOWED_GROUP_IDS.contains(&chat_id.0)
}
