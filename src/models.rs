use serde::{Deserialize, Serialize};

// ==========================================
// 1. The Unified Structure (Used by Handlers)
// ==========================================
#[derive(Debug, Clone)]
pub struct UnifiedDevice {
    pub device_name: String,
    pub codename: String,
    pub maintainer: String,
    pub version: String,
    pub android_version: String,
    pub status: String,
    pub date: String,
    pub size: String,
    pub download_url: String,
    pub security_patch: String,
    pub md5: String,
}

// ==========================================
// 2. MAIN JSON Structure (TheCloverProject.com)
// ==========================================
#[derive(Deserialize, Debug)]
pub struct NewDeviceRoot {
    pub name: String,
    pub codename: String,
    pub maintainer: NewMaintainer,
    pub builds: Vec<NewBuild>,
}

#[derive(Deserialize, Debug)]
pub struct NewMaintainer {
    pub name: String,
}

#[derive(Deserialize, Debug)]
pub struct NewBuild {
    pub version: String,
    #[serde(rename = "androidVersion")]
    pub android_version: String,
    #[serde(rename = "fileSize")]
    pub file_size: String,
    #[serde(rename = "downloadUrl")]
    pub download_url: String,
    pub date: Option<String>, 
    
    // ADDED: Read these from the JSON
    #[serde(rename = "securityPatch")]
    pub security_patch: Option<String>,
    pub md5: Option<String>,
}

// ==========================================
// 3. DATE JSON Structure (official_updates)
// ==========================================
#[derive(Deserialize, Debug)]
pub struct OldDeviceResponse {
    pub response: Vec<OldDeviceInfo>,
}

#[derive(Deserialize, Debug)]
pub struct OldDeviceInfo {
    pub datetime: i64, 
}

// ==========================================
// 4. GitHub API Structure
// ==========================================
#[derive(Deserialize, Debug)]
pub struct GithubFile {
    pub name: String,
}

// ==========================================
// 5. List Cache Structure (NEW)
// ==========================================
// This struct captures the summary info needed for the categorized /list command
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceSummary {
    pub codename: String,
    pub name: String,
    pub brand: String,
}
