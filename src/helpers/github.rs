use anyhow::{Context, Result};
use crate::{
    config,
    helpers::formatting,
    models::{GithubFile, NewDeviceRoot, OldDeviceResponse, UnifiedDevice, DeviceSummary},
    state::AppState,
};

// --- URL GENERATORS ---

fn get_main_device_list_url() -> String {
    format!(
        "https://api.github.com/repos/{}/{}/contents/{}?ref={}",
        config::GITHUB_ORG, config::REPO_MAIN, config::DIR_MAIN, config::BRANCH_MAIN
    )
}

fn get_main_json_url(codename: &str) -> String {
    format!(
        "https://raw.githubusercontent.com/{}/{}/{}/{}/{}.json",
        config::GITHUB_ORG, config::REPO_MAIN, config::BRANCH_MAIN, config::DIR_MAIN, codename
    )
}

fn get_date_json_url(codename: &str) -> String {
    format!(
        "https://raw.githubusercontent.com/{}/{}/{}/{}/{}.json",
        config::GITHUB_ORG, config::REPO_DATE, config::BRANCH_DATE, config::DIR_DATE, codename
    )
}

// --- PRELOAD ---
pub async fn preload_devices(state: &AppState) -> Result<usize> {
    let url = get_main_device_list_url();
    let response = state
        .http_client
        .get(&url)
        .header("User-Agent", "Clover-Bot-Rust")
        .send()
        .await?
        .json::<Vec<GithubFile>>()
        .await?;

    let mut filenames = state.actual_filenames.write().await;
    let mut summaries_cache = state.devices_cache.write().await;

    filenames.clear();
    summaries_cache.clear();

    log::info!("Found {} files. Fetching metadata for list...", response.len());

    for item in response {
        if let Some(device_name) = item.name.strip_suffix(".json") {
            // 1. Store filename mapping
            filenames.insert(device_name.to_lowercase(), device_name.to_string());

            // 2. Fetch the JSON content to get Brand/Name
            // FIXED: Added the missing 5th "{}" here
            let raw_url = format!(
                "https://raw.githubusercontent.com/{}/{}/{}/{}/{}.json",
                config::GITHUB_ORG, config::REPO_MAIN, config::BRANCH_MAIN, config::DIR_MAIN, device_name
            );

            match state.http_client.get(&raw_url).send().await {
                Ok(resp) => {
                    if resp.status().is_success() {
                        // Parse into our summary struct
                        match resp.json::<DeviceSummary>().await {
                            Ok(summary) => {
                                summaries_cache.push(summary);
                            }
                            Err(e) => log::error!("Failed to parse JSON for {}: {}", device_name, e),
                        }
                    } else {
                        log::warn!("Failed to fetch content for {} (HTTP {})", device_name, resp.status());
                    }
                }
                Err(e) => log::error!("Network error fetching {}: {}", device_name, e),
            }
        }
    }
     
    Ok(filenames.len())
}

// --- LOAD DEVICE ---
pub async fn load_device_from_github(
    codename: &str,
    state: &AppState,
) -> Result<Option<UnifiedDevice>> {
    let filenames = state.actual_filenames.read().await;
    let correct_filename = match filenames.get(&codename.to_lowercase()) {
        Some(name) => name,
        None => return Ok(None), 
    };

    // 1. FETCH MAIN DATA
    let main_url = get_main_json_url(correct_filename);
    let main_resp = state.http_client.get(&main_url).send().await?;

    if main_resp.status().is_success() {
        let main_data = main_resp.json::<NewDeviceRoot>().await.context("Failed to parse Main JSON")?;

        if let Some(build) = main_data.builds.first() {
             
            // --- DATE LOGIC ---
            let mut final_date_str = build.date.clone().unwrap_or("Unknown Date".to_string());

            // A. Try to fetch better date from official_updates
            let mut date_url = get_date_json_url(correct_filename);
             
            let mut date_resp_result = state.http_client
                .get(&date_url)
                .header("User-Agent", "Clover-Bot-Rust") 
                .send()
                .await;
             
            // B. FALLBACK LOGIC
            if let Ok(ref resp) = date_resp_result {
                if resp.status() == reqwest::StatusCode::NOT_FOUND {
                    let mut chars = correct_filename.chars();
                    if let Some(first_char) = chars.next() {
                        let cap_filename = first_char.to_uppercase().collect::<String>() + chars.as_str();
                         
                        if cap_filename != *correct_filename {
                            let url_2 = get_date_json_url(&cap_filename);
                             
                            let resp_2 = state.http_client
                                .get(&url_2)
                                .header("User-Agent", "Clover-Bot-Rust")
                                .send()
                                .await;
                                 
                            if resp_2.is_ok() {
                                date_resp_result = resp_2;
                                date_url = url_2; 
                            }
                        }
                    }
                }
            }

            // C. Process Result
            match date_resp_result {
                Ok(resp) => {
                    if resp.status().is_success() {
                         match resp.json::<OldDeviceResponse>().await {
                             Ok(date_data) => {
                                 if let Some(info) = date_data.response.first() {
                                     final_date_str = formatting::format_timestamp(info.datetime);
                                 }
                             },
                             Err(e) => {
                                 log::error!("Failed to parse Date JSON for {}: {}", correct_filename, e);
                             }
                         }
                    } else {
                        log::warn!("Date JSON not found (HTTP {}): {}", resp.status(), date_url);
                    }
                },
                Err(e) => log::warn!("Network error fetching date: {}", e),
            }

            // --- COMBINE DATA ---
            let version_display = if build.version.starts_with('v') {
                build.version.clone()
            } else {
                format!("v{}", build.version)
            };

            // NEW: Fetch status from State
            let statuses = state.device_statuses.read().await;
            let status_text = statuses
                .get(main_data.codename.to_lowercase().as_str())
                .cloned()
                .unwrap_or_else(|| "Active".to_string());

            let device = UnifiedDevice {
                device_name: main_data.name,
                codename: main_data.codename.clone(),
                maintainer: main_data.maintainer.name,
                version: version_display,
                android_version: build.android_version.clone(),
                status: status_text,
                date: final_date_str, 
                size: build.file_size.clone(),
                download_url: build.download_url.clone(),
                security_patch: build.security_patch.clone().unwrap_or("N/A".to_string()),
                md5: build.md5.clone().unwrap_or("N/A".to_string()),
            };
             
            Ok(Some(device))
        } else {
            Ok(None)
        }
    } else {
        Ok(None)
    }
}
