use serde::{Deserialize, Serialize};

// ==========================================
// 1. The Unified Structure (Used by Handlers)
// ==========================================
#[derive(Debug, Clone)]
pub struct UnifiedDevice {
    pub device_name: String,
    pub codename: String,
    pub maintainer: String,
    pub maintainer_github: Option<String>,
    pub version: String,
    pub android_version: String,
    pub status: String,
    pub date: String,
    pub size: String,
    #[allow(dead_code)]
    pub download_url: String,
    pub gms_download_url: Option<String>,
    pub vanilla_download_url: Option<String>,
    pub security_patch: String,
    pub md5: String,
}

// ==========================================
// 2. Site Device JSON Structure (official_updates/site/devices/{codename}.json)
// ==========================================
#[derive(Deserialize, Debug, Clone)]
pub struct SiteDeviceDetail {
    pub name: String,
    #[serde(default)]
    pub brand: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub tagline: Option<String>,
    #[serde(default)]
    pub maintainer: Option<NewMaintainer>,
    #[serde(default, rename = "installationGuide")]
    pub installation_guide: Option<InstallationGuide>,
    #[serde(default, rename = "securityPatch", alias = "security_patch")]
    pub security_patch: Option<String>,
}

// Backward-compatible alias
#[allow(dead_code)]
pub type NewDeviceRoot = SiteDeviceDetail;

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct NewMaintainer {
    pub name: String,
    #[serde(default, rename = "githubUrl")]
    pub github_url: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct InstallationGuide {
    #[serde(default)]
    pub steps: Vec<InstallationStep>,
    #[serde(default)]
    #[allow(dead_code)]
    pub warning: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct InstallationStep {
    pub title: String,
    pub description: String,
}

// ==========================================
// 3. OTA / Update JSON Structure (official_updates/devices/...)
// ==========================================
#[derive(Deserialize, Debug, Clone)]
pub struct OldDeviceResponse {
    pub response: Vec<OldDeviceInfo>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct OldDeviceInfo {
    pub datetime: i64,
    #[allow(dead_code)]
    pub filename: Option<String>,
    #[allow(dead_code)]
    pub id: Option<String>,
    pub md5: Option<String>,
    pub size: Option<serde_json::Value>,
    pub url: Option<String>,
    pub version: Option<String>,
    pub devicename: Option<String>,
    pub maintainer: Option<String>,
    #[serde(default, rename = "romtype")]
    #[allow(dead_code)]
    pub rom_type: Option<String>,
    #[serde(default, rename = "securityPatch", alias = "security_patch")]
    pub security_patch: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ResolvedUpdates {
    pub latest_datetime: Option<i64>,
    pub gms_url: Option<String>,
    pub vanilla_url: Option<String>,
    pub version: Option<String>,
    pub md5: Option<String>,
    pub size_formatted: Option<String>,
    pub android_version: String,
    pub devicename: Option<String>,
    pub maintainer: Option<String>,
    pub download_url: Option<String>,
    pub security_patch: Option<String>,
}

// ==========================================
// 4. GitHub API Structure
// ==========================================
#[derive(Deserialize, Debug)]
pub struct GithubFile {
    pub name: String,
}

// ==========================================
// 5. List Cache Structure (official_updates/site/devices.json)
// ==========================================
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceSummary {
    pub codename: String,
    pub name: String,
    pub brand: String,
    #[serde(default)]
    pub maintainer: Option<NewMaintainer>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub datetime: Option<i64>,
    #[serde(default)]
    pub variants: Option<Vec<String>>,
    #[serde(default)]
    pub changelog: Option<String>,
    #[serde(default)]
    pub released: Option<bool>,
}
