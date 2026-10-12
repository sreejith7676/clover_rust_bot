use std::collections::HashSet;
use std::str::FromStr;

use anyhow::{bail, Context, Result};
use teloxide::types::{ChatId, UserId};

// Links
pub const GROUP_LINK: &str = "https://t.me/TheCloverProject_Chat";
pub const WEBSITE_URL: &str = "https://thecloverproject.com";

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

// Local files (relative to the working directory)
pub const LOG_FILE: &str = "clover_bot.log";
pub const STATUS_FILE: &str = "device_status.json";

/// Runtime configuration loaded from the environment (or a `.env` file).
///
/// | Variable             | Required | Meaning                                          |
/// |----------------------|----------|--------------------------------------------------|
/// | `TELOXIDE_TOKEN`     | yes      | Telegram bot token (read in `main`)              |
/// | `ADMIN_IDS`          | yes      | Comma-separated Telegram user IDs of admins      |
/// | `ALLOWED_GROUP_IDS`  | yes      | Comma-separated chat IDs the bot may be used in  |
/// | `SPECIAL_GROUP_ID`   | no       | Chat ID that may use /uptime /ping /stats /server|
/// | `GITHUB_TOKEN`       | no       | Raises the GitHub API rate limit (fallback path) |
#[derive(Debug, Clone, Default)]
pub struct Config {
    pub admin_ids: HashSet<u64>,
    pub allowed_groups: HashSet<i64>,
    pub special_group: Option<ChatId>,
    pub github_token: Option<String>,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let admin_ids: HashSet<u64> = env_list("ADMIN_IDS")?.into_iter().collect();
        let allowed_groups: HashSet<i64> = env_list("ALLOWED_GROUP_IDS")?.into_iter().collect();

        if admin_ids.is_empty() {
            bail!("ADMIN_IDS must contain at least one Telegram user ID");
        }
        if allowed_groups.is_empty() {
            bail!("ALLOWED_GROUP_IDS must contain at least one chat ID");
        }

        let special_group = match std::env::var("SPECIAL_GROUP_ID") {
            Ok(v) if !v.trim().is_empty() => Some(ChatId(
                v.trim()
                    .parse::<i64>()
                    .with_context(|| format!("SPECIAL_GROUP_ID is not a valid chat ID: {v:?}"))?,
            )),
            _ => None,
        };

        let github_token = std::env::var("GITHUB_TOKEN")
            .ok()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty());

        Ok(Self {
            admin_ids,
            allowed_groups,
            special_group,
            github_token,
        })
    }

    pub fn is_admin(&self, user_id: UserId) -> bool {
        self.admin_ids.contains(&user_id.0)
    }

    pub fn is_allowed(&self, chat_id: ChatId) -> bool {
        self.allowed_groups.contains(&chat_id.0)
    }

    /// Admins anywhere, plus everyone in the special group, may use server commands.
    pub fn is_special(&self, chat_id: ChatId, user_id: UserId) -> bool {
        self.is_admin(user_id) || self.special_group == Some(chat_id)
    }
}

fn env_list<T>(key: &str) -> Result<Vec<T>>
where
    T: FromStr,
    T::Err: std::fmt::Display,
{
    match std::env::var(key) {
        Ok(raw) => parse_list(&raw).with_context(|| format!("invalid value for {key}")),
        Err(_) => bail!("{key} is not set (see .env.example)"),
    }
}

pub fn parse_list<T>(raw: &str) -> Result<Vec<T>>
where
    T: FromStr,
    T::Err: std::fmt::Display,
{
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            s.parse::<T>()
                .map_err(|e| anyhow::anyhow!("{s:?} is not valid: {e}"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_comma_separated_ids() {
        assert_eq!(parse_list::<i64>("-1001, -1002 ,").unwrap(), vec![-1001, -1002]);
        assert_eq!(parse_list::<u64>("42").unwrap(), vec![42]);
        assert!(parse_list::<u64>("abc").is_err());
        assert!(parse_list::<u64>("").unwrap().is_empty());
    }

    #[test]
    fn special_chat_rules() {
        let cfg = Config {
            admin_ids: [1].into_iter().collect(),
            allowed_groups: [-10, -20].into_iter().collect(),
            special_group: Some(ChatId(-20)),
            github_token: None,
        };
        assert!(cfg.is_special(ChatId(-10), UserId(1))); // admin anywhere
        assert!(cfg.is_special(ChatId(-20), UserId(2))); // anyone in special group
        assert!(!cfg.is_special(ChatId(-10), UserId(2)));
        assert!(cfg.is_allowed(ChatId(-10)) && !cfg.is_allowed(ChatId(-30)));
    }
}
