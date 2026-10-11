use anyhow::{Context, Result};
use futures::stream::{self, StreamExt};
use crate::{
    config,
    helpers::formatting,
    models::{
        DeviceSummary, GithubFile, OldDeviceInfo, OldDeviceResponse, ResolvedUpdates,
        SiteDeviceDetail, UnifiedDevice,
    },
    state::AppState,
};

// --- URL GENERATORS ---

fn get_site_devices_url() -> String {
    format!(
        "https://raw.githubusercontent.com/{}/{}/{}/{}",
        config::GITHUB_ORG,
        config::REPO_UPDATES,
        config::DEFAULT_BRANCH,
        config::SITE_DEVICES_JSON
    )
}

fn get_site_devices_contents_api_url() -> String {
    format!(
        "https://api.github.com/repos/{}/{}/contents/{}?ref={}",
        config::GITHUB_ORG,
        config::REPO_UPDATES,
        config::DIR_SITE_DEVICES,
        config::DEFAULT_BRANCH
    )
}

fn get_site_device_json_url(codename: &str) -> String {
    format!(
        "https://raw.githubusercontent.com/{}/{}/{}/{}/{}.json",
        config::GITHUB_ORG,
        config::REPO_UPDATES,
        config::DEFAULT_BRANCH,
        config::DIR_SITE_DEVICES,
        codename
    )
}

// --- PRELOAD ---
pub async fn preload_devices(state: &AppState) -> Result<usize> {
    // 1. Primary approach: fetch unified site/devices.json (fast, single HTTP request)
    let site_url = get_site_devices_url();
    let resp = state
        .http_client
        .get(&site_url)
        .header("User-Agent", "Clover-Bot-Rust")
        .send()
        .await;

    if let Ok(resp) = resp {
        if resp.status().is_success() {
            if let Ok(summaries) = resp.json::<Vec<DeviceSummary>>().await {
                let total = summaries.len();

                // Store filename mapping
                {
                    let mut filenames = state.actual_filenames.write().await;
                    filenames.clear();
                    for item in &summaries {
                        filenames.insert(item.codename.to_lowercase(), item.codename.clone());
                    }
                }

                // Store in cache
                {
                    let mut cache = state.devices_cache.write().await;
                    *cache = summaries;
                }

                log::info!("Preloaded {} devices from site/devices.json successfully into cache.", total);
                return Ok(total);
            }
        }
    }

    // 2. Fallback approach: query GitHub contents API for site/devices directory
    log::warn!("site/devices.json direct fetch failed; falling back to GitHub contents API...");
    let api_url = get_site_devices_contents_api_url();
    let response = state
        .http_client
        .get(&api_url)
        .header("User-Agent", "Clover-Bot-Rust")
        .send()
        .await?
        .json::<Vec<GithubFile>>()
        .await?;

    let device_names: Vec<String> = response
        .into_iter()
        .filter_map(|item| item.name.strip_suffix(".json").map(|s| s.to_string()))
        .collect();

    let total_found = device_names.len();

    {
        let mut filenames = state.actual_filenames.write().await;
        filenames.clear();
        for name in &device_names {
            filenames.insert(name.to_lowercase(), name.clone());
        }
    }

    log::info!("Found {} files via API. Fetching device details concurrently...", total_found);

    let client = state.http_client.clone();
    let summaries: Vec<DeviceSummary> = stream::iter(device_names)
        .map(|device_name| {
            let client = client.clone();
            async move {
                let raw_url = get_site_device_json_url(&device_name);
                match client.get(&raw_url).header("User-Agent", "Clover-Bot-Rust").send().await {
                    Ok(resp) if resp.status().is_success() => {
                        match resp.json::<SiteDeviceDetail>().await {
                            Ok(detail) => Some(DeviceSummary {
                                codename: device_name,
                                name: detail.name,
                                brand: detail.brand.unwrap_or_else(|| "Unknown".to_string()),
                                maintainer: detail.maintainer,
                                version: None,
                                datetime: None,
                                variants: None,
                                changelog: None,
                                released: None,
                            }),
                            Err(e) => {
                                log::warn!("Failed to parse SiteDeviceDetail for {}: {}", device_name, e);
                                None
                            }
                        }
                    }
                    _ => None,
                }
            }
        })
        .buffer_unordered(6)
        .filter_map(|opt| async move { opt })
        .collect()
        .await;

    let loaded_count = summaries.len();
    {
        let mut cache = state.devices_cache.write().await;
        *cache = summaries;
    }

    log::info!("Preloaded {} device summaries successfully into cache via fallback.", loaded_count);
    Ok(loaded_count)
}

// --- FETCH UPDATE FROM OFFICIAL_UPDATES ---
async fn fetch_single_update_json(
    client: &reqwest::Client,
    url: &str,
) -> Option<OldDeviceInfo> {
    match client
        .get(url)
        .header("User-Agent", "Clover-Bot-Rust")
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            match resp.json::<OldDeviceResponse>().await {
                Ok(data) => data.response.into_iter().next(),
                Err(e) => {
                    log::debug!("Could not deserialize update response from {}: {}", url, e);
                    None
                }
            }
        }
        _ => None,
    }
}

pub async fn fetch_official_update_info(
    client: &reqwest::Client,
    codename: &str,
) -> Option<ResolvedUpdates> {
    let lower = codename.to_lowercase();
    let mut capitalized = String::new();
    let mut chars = lower.chars();
    if let Some(first) = chars.next() {
        capitalized.push_str(&first.to_uppercase().to_string());
        capitalized.push_str(chars.as_str());
    }

    let name_variants: Vec<&str> = if capitalized != lower {
        vec![lower.as_str(), capitalized.as_str()]
    } else {
        vec![lower.as_str()]
    };

    // Inspect branches in order: 17 (A17), 16 (A16), 15 (A15)
    for branch in config::ALL_BRANCHES {
        for name in &name_variants {
            // A. Concurrently query gms and vanilla subdirectories
            let gms_url = format!(
                "https://raw.githubusercontent.com/{}/{}/{}/devices/gms/{}.json",
                config::GITHUB_ORG, config::REPO_UPDATES, branch, name
            );
            let vanilla_url = format!(
                "https://raw.githubusercontent.com/{}/{}/{}/devices/vanilla/{}.json",
                config::GITHUB_ORG, config::REPO_UPDATES, branch, name
            );

            let (gms_res, vanilla_res) = tokio::join!(
                fetch_single_update_json(client, &gms_url),
                fetch_single_update_json(client, &vanilla_url)
            );

            if gms_res.is_some() || vanilla_res.is_some() {
                let mut resolved = ResolvedUpdates {
                    android_version: branch.to_string(),
                    ..Default::default()
                };

                if let Some(ref gms) = gms_res {
                    resolved.gms_url = gms.url.clone();
                    resolved.latest_datetime = Some(resolved.latest_datetime.map_or(gms.datetime, |d| d.max(gms.datetime)));
                    if resolved.version.is_none() {
                        resolved.version = gms.version.clone();
                    }
                    if resolved.md5.is_none() {
                        resolved.md5 = gms.md5.clone();
                    }
                    if resolved.size_formatted.is_none() {
                        if let Some(ref sz) = gms.size {
                            resolved.size_formatted = Some(formatting::format_size_value(sz));
                        }
                    }
                    if resolved.devicename.is_none() {
                        resolved.devicename = gms.devicename.clone();
                    }
                    if resolved.maintainer.is_none() {
                        resolved.maintainer = gms.maintainer.clone();
                    }
                    if resolved.security_patch.is_none() {
                        resolved.security_patch = gms.security_patch.clone();
                    }
                }

                if let Some(ref vanilla) = vanilla_res {
                    resolved.vanilla_url = vanilla.url.clone();
                    resolved.latest_datetime = Some(resolved.latest_datetime.map_or(vanilla.datetime, |d| d.max(vanilla.datetime)));
                    if resolved.version.is_none() {
                        resolved.version = vanilla.version.clone();
                    }
                    if resolved.md5.is_none() {
                        resolved.md5 = vanilla.md5.clone();
                    }
                    if resolved.size_formatted.is_none() {
                        if let Some(ref sz) = vanilla.size {
                            resolved.size_formatted = Some(formatting::format_size_value(sz));
                        }
                    }
                    if resolved.devicename.is_none() {
                        resolved.devicename = vanilla.devicename.clone();
                    }
                    if resolved.maintainer.is_none() {
                        resolved.maintainer = vanilla.maintainer.clone();
                    }
                    if resolved.security_patch.is_none() {
                        resolved.security_patch = vanilla.security_patch.clone();
                    }
                }

                resolved.download_url = resolved.gms_url.clone().or_else(|| resolved.vanilla_url.clone());

                log::debug!(
                    "Found update for {} on branch {}: GMS={:?}, Vanilla={:?}",
                    name, branch, resolved.gms_url.is_some(), resolved.vanilla_url.is_some()
                );
                return Some(resolved);
            }

            // B. Fallback: Query legacy root devices/{name}.json
            let legacy_url = format!(
                "https://raw.githubusercontent.com/{}/{}/{}/devices/{}.json",
                config::GITHUB_ORG, config::REPO_UPDATES, branch, name
            );
            if let Some(info) = fetch_single_update_json(client, &legacy_url).await {
                log::debug!("Found update in legacy directory for {} on branch {}", name, branch);
                let size_str = info.size.as_ref().map(formatting::format_size_value);
                let resolved = ResolvedUpdates {
                    latest_datetime: Some(info.datetime),
                    gms_url: None,
                    vanilla_url: None,
                    version: info.version,
                    md5: info.md5,
                    size_formatted: size_str,
                    android_version: branch.to_string(),
                    devicename: info.devicename,
                    maintainer: info.maintainer,
                    download_url: info.url,
                    security_patch: info.security_patch,
                };
                return Some(resolved);
            }
        }
    }

    log::debug!("No update entry found for {} across branches 17, 16, 15", codename);
    None
}

// --- FETCH SITE DEVICE INFO (used by /flash) ---
pub async fn fetch_device_site_info(
    codename: &str,
    state: &AppState,
) -> Result<Option<SiteDeviceDetail>> {
    let clean_codename = codename.trim();
    let filenames = state.actual_filenames.read().await;
    let correct_filename = filenames
        .get(&clean_codename.to_lowercase())
        .cloned()
        .unwrap_or_else(|| clean_codename.to_string());
    drop(filenames);

    let site_url = get_site_device_json_url(&correct_filename);
    let resp = state
        .http_client
        .get(&site_url)
        .header("User-Agent", "Clover-Bot-Rust")
        .send()
        .await?;

    if !resp.status().is_success() {
        return Ok(None);
    }

    let detail = resp.json::<SiteDeviceDetail>().await.context("Failed to parse Site Device Detail")?;
    Ok(Some(detail))
}

// --- FETCH CHANGELOG (used by /changelog) ---
pub async fn fetch_device_changelog(
    codename: &str,
    state: &AppState,
) -> Result<Option<String>> {
    let clean_codename = codename.trim();
    let filenames = state.actual_filenames.read().await;
    let correct_filename = filenames
        .get(&clean_codename.to_lowercase())
        .cloned()
        .unwrap_or_else(|| clean_codename.to_string());
    drop(filenames);

    // Look up changelog filename from cached summary
    let cached_file = {
        let cache = state.devices_cache.read().await;
        cache.iter()
            .find(|d| d.codename.eq_ignore_ascii_case(&correct_filename))
            .and_then(|d| d.changelog.clone())
    };

    let mut candidates: Vec<String> = Vec::new();
    if let Some(cf) = cached_file {
        candidates.push(cf);
    }

    let lower = correct_filename.to_lowercase();
    let mut capitalized = String::new();
    let mut chars = lower.chars();
    if let Some(first) = chars.next() {
        capitalized.push_str(&first.to_uppercase().to_string());
        capitalized.push_str(chars.as_str());
    }

    candidates.push(format!("changelog_{}.txt", lower));
    if capitalized != lower {
        candidates.push(format!("changelog_{}.txt", capitalized));
    }

    for branch in config::ALL_BRANCHES {
        for candidate in &candidates {
            let url = format!(
                "https://raw.githubusercontent.com/{}/{}/{}/{}/{}",
                config::GITHUB_ORG, config::REPO_UPDATES, branch, config::DIR_CHANGELOGS, candidate
            );

            if let Ok(resp) = state.http_client.get(&url).header("User-Agent", "Clover-Bot-Rust").send().await {
                if resp.status().is_success() {
                    if let Ok(text) = resp.text().await {
                        if !text.trim().is_empty() {
                            return Ok(Some(text));
                        }
                    }
                }
            }
        }
    }

    Ok(None)
}

// --- LOAD DEVICE (used by /clover) ---
pub async fn load_device_from_github(
    codename: &str,
    state: &AppState,
) -> Result<Option<UnifiedDevice>> {
    let clean_codename = codename.trim();
    let filenames = state.actual_filenames.read().await;
    let correct_filename = filenames
        .get(&clean_codename.to_lowercase())
        .cloned()
        .unwrap_or_else(|| clean_codename.to_string());
    drop(filenames);

    // 1. Look up cached DeviceSummary
    let cached_summary = {
        let cache = state.devices_cache.read().await;
        cache.iter()
            .find(|d| d.codename.eq_ignore_ascii_case(&correct_filename))
            .cloned()
    };

    // 2. Fetch updates across branches 17, 16, 15
    let updates = fetch_official_update_info(&state.http_client, &correct_filename).await;

    // 3. Fallback to site detail if needed
    let site_detail = if cached_summary.is_none() {
        fetch_device_site_info(&correct_filename, state).await.unwrap_or(None)
    } else {
        None
    };

    // If device doesn't exist in cache, site, or updates
    if cached_summary.is_none() && updates.is_none() && site_detail.is_none() {
        return Ok(None);
    }

    // Resolve device name
    let device_name = cached_summary
        .as_ref()
        .map(|s| s.name.clone())
        .or_else(|| site_detail.as_ref().map(|d| d.name.clone()))
        .or_else(|| updates.as_ref().and_then(|u| u.devicename.clone()))
        .unwrap_or_else(|| correct_filename.clone());

    // Resolve maintainer
    let maintainer = cached_summary
        .as_ref()
        .and_then(|s| s.maintainer.as_ref().map(|m| m.name.clone()))
        .or_else(|| site_detail.as_ref().and_then(|d| d.maintainer.as_ref().map(|m| m.name.clone())))
        .or_else(|| updates.as_ref().and_then(|u| u.maintainer.clone()))
        .unwrap_or_else(|| "Unknown".to_string());

    let maintainer_github = cached_summary
        .as_ref()
        .and_then(|s| s.maintainer.as_ref().and_then(|m| m.github_url.clone()))
        .or_else(|| site_detail.as_ref().and_then(|d| d.maintainer.as_ref().and_then(|m| m.github_url.clone())));

    // Resolve version
    let raw_version = updates
        .as_ref()
        .and_then(|u| u.version.clone())
        .or_else(|| cached_summary.as_ref().and_then(|s| s.version.clone()));

    let version_display = match raw_version {
        Some(ref v) if v.starts_with('v') => v.clone(),
        Some(ref v) => format!("v{}", v),
        None => "Upcoming".to_string(),
    };

    // Resolve Android version
    let android_version = updates
        .as_ref()
        .map(|u| u.android_version.clone())
        .unwrap_or_else(|| "N/A".to_string());

    // Resolve date
    let date_str = updates
        .as_ref()
        .and_then(|u| u.latest_datetime)
        .map(formatting::format_timestamp)
        .or_else(|| {
            cached_summary
                .as_ref()
                .and_then(|s| s.datetime)
                .map(formatting::format_timestamp)
        })
        .unwrap_or_else(|| "N/A".to_string());

    // Resolve size
    let size_str = updates
        .as_ref()
        .and_then(|u| u.size_formatted.clone())
        .unwrap_or_else(|| "N/A".to_string());

    // Download URLs: ONLY populated for active Android 17 builds.
    // Builds below A17 have dead links and devices without update entry have no build.
    let is_a17 = android_version == "17";
    let (gms_download_url, vanilla_download_url, download_url) = if is_a17 {
        let gms = updates.as_ref().and_then(|u| u.gms_url.clone());
        let vanilla = updates.as_ref().and_then(|u| u.vanilla_url.clone());
        let dl = updates
            .as_ref()
            .and_then(|u| u.download_url.clone())
            .unwrap_or_else(|| format!("https://thecloverproject.com/download/{}", correct_filename));
        (gms, vanilla, dl)
    } else {
        (None, None, String::new())
    };

    let md5 = updates
        .as_ref()
        .and_then(|u| u.md5.clone())
        .unwrap_or_else(|| "N/A".to_string());

    // Status: Custom override > Unreleased check > Active
    let statuses = state.device_statuses.read().await;
    let status_text = statuses
        .get(&correct_filename.to_lowercase())
        .cloned()
        .unwrap_or_else(|| {
            if cached_summary.as_ref().and_then(|s| s.released) == Some(false) {
                "Upcoming".to_string()
            } else {
                "Active".to_string()
            }
        });

    // Resolve security patch: explicit in update/site > derived from build datetime (%B %Y) > "N/A"
    let security_patch = updates
        .as_ref()
        .and_then(|u| u.security_patch.clone())
        .or_else(|| site_detail.as_ref().and_then(|d| d.security_patch.clone()))
        .or_else(|| {
            updates
                .as_ref()
                .and_then(|u| u.latest_datetime)
                .and_then(|dt| {
                    chrono::DateTime::from_timestamp(dt, 0)
                        .map(|d| d.format("%B %Y").to_string())
                })
        })
        .or_else(|| {
            cached_summary
                .as_ref()
                .and_then(|s| s.datetime)
                .and_then(|dt| {
                    chrono::DateTime::from_timestamp(dt, 0)
                        .map(|d| d.format("%B %Y").to_string())
                })
        })
        .unwrap_or_else(|| "N/A".to_string());

    let device = UnifiedDevice {
        device_name,
        codename: correct_filename,
        maintainer,
        maintainer_github,
        version: version_display,
        android_version,
        status: status_text,
        date: date_str,
        size: size_str,
        download_url,
        gms_download_url,
        vanilla_download_url,
        security_patch,
        md5,
    };

    Ok(Some(device))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_fetch_official_update_peridot_a17() {
        let client = reqwest::Client::new();
        let updates = fetch_official_update_info(&client, "peridot").await;
        assert!(updates.is_some(), "Expected to find update for peridot on branch 17");
        let updates = updates.unwrap();
        assert_eq!(updates.android_version, "17");
        assert!(updates.latest_datetime.is_some());
        assert!(updates.gms_url.is_some());
        assert!(updates.vanilla_url.is_some());
    }

    #[tokio::test]
    async fn test_fetch_official_update_cancunf_a16() {
        let client = reqwest::Client::new();
        let updates = fetch_official_update_info(&client, "cancunf").await;
        assert!(updates.is_some(), "Expected to find update for cancunf on branch 16");
        let updates = updates.unwrap();
        assert_eq!(updates.android_version, "16");
        assert!(updates.latest_datetime.is_some());
        assert!(updates.gms_url.is_some());
    }

    #[tokio::test]
    async fn test_fetch_official_update_fog_a15() {
        let client = reqwest::Client::new();
        let updates = fetch_official_update_info(&client, "fog").await;
        assert!(updates.is_some(), "Expected to find update for fog on branch 15");
        let updates = updates.unwrap();
        assert_eq!(updates.android_version, "15");
        assert!(updates.latest_datetime.is_some());
    }

    #[tokio::test]
    async fn test_preload_devices_from_site_devices_json() {
        use std::collections::HashMap;
        use std::sync::Arc;
        let state = AppState::new(
            HashMap::new(),
            crate::helpers::logs::LogBuffer::new(100),
            Arc::new(crate::helpers::logs::ListenerErrorHandler::new()),
        );

        let count = preload_devices(&state).await.expect("Preload should succeed");
        assert!(count >= 20, "Expected at least 20 devices, got {}", count);

        let cache = state.devices_cache.read().await;
        assert!(cache.iter().any(|d| d.codename == "peridot"));
        assert!(cache.iter().any(|d| d.codename == "cancunf"));
    }

    #[tokio::test]
    async fn test_fetch_device_changelog() {
        use std::collections::HashMap;
        use std::sync::Arc;
        let state = AppState::new(
            HashMap::new(),
            crate::helpers::logs::LogBuffer::new(100),
            Arc::new(crate::helpers::logs::ListenerErrorHandler::new()),
        );

        let _ = preload_devices(&state).await;

        let log_peridot = fetch_device_changelog("peridot", &state).await.expect("fetch should succeed");
        assert!(log_peridot.is_some(), "Expected changelog for peridot");

        let log_cancunf = fetch_device_changelog("cancunf", &state).await.expect("fetch should succeed");
        assert!(log_cancunf.is_some(), "Expected changelog for cancunf");
    }

    #[tokio::test]
    async fn test_fetch_device_site_info() {
        use std::collections::HashMap;
        use std::sync::Arc;
        let state = AppState::new(
            HashMap::new(),
            crate::helpers::logs::LogBuffer::new(100),
            Arc::new(crate::helpers::logs::ListenerErrorHandler::new()),
        );

        let _ = preload_devices(&state).await;

        let detail = fetch_device_site_info("peridot", &state).await.expect("fetch should succeed");
        assert!(detail.is_some(), "Expected site detail for peridot");
        let detail = detail.unwrap();
        assert!(detail.installation_guide.is_some());
        assert!(!detail.installation_guide.unwrap().steps.is_empty());
    }

    #[tokio::test]
    async fn test_load_device_from_github() {
        use std::collections::HashMap;
        use std::sync::Arc;
        let state = AppState::new(
            HashMap::new(),
            crate::helpers::logs::LogBuffer::new(100),
            Arc::new(crate::helpers::logs::ListenerErrorHandler::new()),
        );

        let _ = preload_devices(&state).await;

        let dev = load_device_from_github("peridot", &state).await.expect("load should succeed");
        assert!(dev.is_some(), "Expected UnifiedDevice for peridot");
        let dev = dev.unwrap();
        assert_eq!(dev.codename, "peridot");
        assert_eq!(dev.android_version, "17");
        assert_eq!(dev.security_patch, "October 2026");
        assert!(dev.gms_download_url.is_some());
        assert!(dev.vanilla_download_url.is_some());

        let dev_cancunf = load_device_from_github("cancunf", &state).await.expect("load should succeed");
        assert!(dev_cancunf.is_some(), "Expected UnifiedDevice for cancunf");
        let dev_cancunf = dev_cancunf.unwrap();
        assert_eq!(dev_cancunf.codename, "cancunf");
        assert_eq!(dev_cancunf.android_version, "16");
        assert_eq!(dev_cancunf.security_patch, "June 2026");
        assert!(dev_cancunf.gms_download_url.is_none(), "Download button should be removed for A16");
        assert!(dev_cancunf.vanilla_download_url.is_none(), "Download button should be removed for A16");

        let dev_vayu = load_device_from_github("vayu", &state).await.expect("load should succeed");
        assert!(dev_vayu.is_some(), "Expected UnifiedDevice for vayu");
        let dev_vayu = dev_vayu.unwrap();
        assert_eq!(dev_vayu.codename, "vayu");
        assert_eq!(dev_vayu.android_version, "N/A");
        assert_eq!(dev_vayu.version, "Upcoming");
        assert!(dev_vayu.gms_download_url.is_none(), "Download button should be removed when no entry");
        assert!(dev_vayu.vanilla_download_url.is_none(), "Download button should be removed when no entry");
    }
}
