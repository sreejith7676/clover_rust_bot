use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use futures::stream::{self, StreamExt};

use crate::{
    config,
    helpers::{
        formatting,
        text::{capitalize, valid_codename},
    },
    models::{
        DeviceSummary, GithubFile, OldDeviceInfo, OldDeviceResponse, ResolvedUpdates,
        SiteDeviceDetail, UnifiedDevice,
    },
    state::{AppState, UPDATE_CACHE_TTL},
};

const RAW_BASE: &str = "https://raw.githubusercontent.com";

/// How often the catalog is refreshed in the background (and how soon to retry if empty).
const CATALOG_REFRESH: Duration = Duration::from_secs(30 * 60);
const CATALOG_RETRY: Duration = Duration::from_secs(60);

// --- URL GENERATORS ---

fn raw_url(branch: &str, path: &str) -> String {
    format!(
        "{}/{}/{}/{}/{}",
        RAW_BASE,
        config::GITHUB_ORG,
        config::REPO_UPDATES,
        branch,
        path
    )
}

fn site_devices_url() -> String {
    raw_url(config::DEFAULT_BRANCH, config::SITE_DEVICES_JSON)
}

fn site_device_json_url(codename: &str) -> String {
    raw_url(
        config::DEFAULT_BRANCH,
        &format!("{}/{}.json", config::DIR_SITE_DEVICES, codename),
    )
}

fn site_devices_contents_api_url() -> String {
    format!(
        "https://api.github.com/repos/{}/{}/contents/{}?ref={}",
        config::GITHUB_ORG,
        config::REPO_UPDATES,
        config::DIR_SITE_DEVICES,
        config::DEFAULT_BRANCH
    )
}

/// Map user input to the canonical (correctly-cased) filename if we know it.
async fn resolve_codename(state: &AppState, input: &str) -> String {
    let clean = input.trim();
    state
        .actual_filenames
        .read()
        .await
        .get(&clean.to_lowercase())
        .cloned()
        .unwrap_or_else(|| clean.to_string())
}

fn name_variants(codename: &str) -> Vec<String> {
    let lower = codename.to_lowercase();
    let capitalized = capitalize(&lower);
    if capitalized != lower {
        vec![lower, capitalized]
    } else {
        vec![lower]
    }
}

// --- PRELOAD ---

/// Replace the catalog and filename map with `summaries`.
async fn apply_catalog(state: &AppState, summaries: Vec<DeviceSummary>) -> usize {
    let total = summaries.len();
    {
        let mut filenames = state.actual_filenames.write().await;
        filenames.clear();
        for item in &summaries {
            filenames.insert(item.codename.to_lowercase(), item.codename.clone());
        }
    }
    *state.devices_cache.write().await = summaries;
    // Catalog changed: drop cached update lookups so /sync takes effect immediately.
    state.update_cache.write().await.clear();
    total
}

pub async fn preload_devices(state: &AppState) -> Result<usize> {
    // 1. Primary approach: unified site/devices.json (a single HTTP request)
    match preload_from_site_json(state).await {
        Ok(n) => {
            log::info!("Preloaded {} devices from site/devices.json.", n);
            return Ok(n);
        }
        Err(e) => log::warn!(
            "site/devices.json fetch failed ({:#}); falling back to GitHub contents API...",
            e
        ),
    }

    // 2. Fallback approach: query the GitHub contents API for site/devices
    let n = preload_from_contents_api(state).await?;
    log::info!("Preloaded {} device summaries via the contents-API fallback.", n);
    Ok(n)
}

async fn preload_from_site_json(state: &AppState) -> Result<usize> {
    let summaries = state
        .http_client
        .get(site_devices_url())
        .send()
        .await
        .context("request failed")?
        .error_for_status()
        .context("unexpected HTTP status")?
        .json::<Vec<DeviceSummary>>()
        .await
        .context("response is not a valid device list")?;

    if summaries.is_empty() {
        bail!("device list is empty");
    }
    Ok(apply_catalog(state, summaries).await)
}

async fn preload_from_contents_api(state: &AppState) -> Result<usize> {
    let mut request = state
        .http_client
        .get(site_devices_contents_api_url())
        .header("Accept", "application/vnd.github+json");
    if let Some(token) = &state.config.github_token {
        request = request.bearer_auth(token);
    }

    let files = request
        .send()
        .await
        .context("GitHub API request failed")?
        .error_for_status()
        .context("GitHub API returned an error (rate limited? set GITHUB_TOKEN)")?
        .json::<Vec<GithubFile>>()
        .await
        .context("GitHub API response is not a file list")?;

    let device_names: Vec<String> = files
        .into_iter()
        .filter_map(|f| f.name.strip_suffix(".json").map(str::to_string))
        .filter(|n| valid_codename(n))
        .collect();

    log::info!(
        "Found {} files via API. Fetching device details concurrently...",
        device_names.len()
    );

    let client = state.http_client.clone();
    let summaries: Vec<DeviceSummary> = stream::iter(device_names)
        .map(|device_name| {
            let client = client.clone();
            async move {
                let resp = match client.get(site_device_json_url(&device_name)).send().await {
                    Ok(r) if r.status().is_success() => r,
                    Ok(r) => {
                        log::warn!("{}: HTTP {}", device_name, r.status());
                        return None;
                    }
                    Err(e) => {
                        log::warn!("{}: request failed: {}", device_name, e);
                        return None;
                    }
                };
                match resp.json::<SiteDeviceDetail>().await {
                    Ok(detail) => Some(DeviceSummary {
                        codename: device_name,
                        name: detail.name,
                        brand: detail.brand.unwrap_or_else(|| "Unknown".to_string()),
                        maintainer: detail.maintainer,
                        version: None,
                        datetime: None,
                        changelog: None,
                        released: None,
                    }),
                    Err(e) => {
                        log::warn!("Failed to parse SiteDeviceDetail for {}: {}", device_name, e);
                        None
                    }
                }
            }
        })
        .buffer_unordered(6)
        .filter_map(|opt| async move { opt })
        .collect()
        .await;

    if summaries.is_empty() {
        bail!("no device details could be loaded");
    }
    Ok(apply_catalog(state, summaries).await)
}

/// Keeps the catalog fresh: refreshes periodically, and retries quickly while it is empty
/// (e.g. when GitHub was unreachable at startup). A failed refresh keeps the old data.
pub fn spawn_catalog_refresh(state: std::sync::Arc<AppState>) {
    tokio::spawn(async move {
        loop {
            let delay = if state.devices_cache.read().await.is_empty() {
                CATALOG_RETRY
            } else {
                CATALOG_REFRESH
            };
            tokio::time::sleep(delay).await;

            match preload_devices(&state).await {
                Ok(n) => log::debug!("Background catalog refresh OK ({} devices).", n),
                Err(e) => log::warn!("Background catalog refresh failed: {:#}", e),
            }
        }
    });
}

// --- FETCH UPDATE FROM OFFICIAL_UPDATES ---

/// Result of a single JSON lookup. `Failed` (network error, 5xx, bad JSON) is kept
/// distinct from `NotFound` so transient errors are never cached as "no update".
enum Lookup {
    Found(Box<OldDeviceInfo>),
    NotFound,
    Failed,
}

async fn fetch_single_update_json(client: &reqwest::Client, url: &str) -> Lookup {
    let resp = match client.get(url).send().await {
        Ok(r) => r,
        Err(e) => {
            log::debug!("Request to {} failed: {}", url, e);
            return Lookup::Failed;
        }
    };

    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Lookup::NotFound;
    }
    if !resp.status().is_success() {
        log::debug!("{} returned HTTP {}", url, resp.status());
        return Lookup::Failed;
    }

    match resp.json::<OldDeviceResponse>().await {
        Ok(data) => match data.response.into_iter().next() {
            Some(info) => Lookup::Found(Box::new(info)),
            None => Lookup::NotFound,
        },
        Err(e) => {
            log::debug!("Could not deserialize update response from {}: {}", url, e);
            Lookup::Failed
        }
    }
}

/// Fill any still-empty fields of `r` from `info` (first source wins; newest date wins).
fn merge_info(r: &mut ResolvedUpdates, info: &OldDeviceInfo) {
    r.latest_datetime = Some(r.latest_datetime.map_or(info.datetime, |d| d.max(info.datetime)));
    if r.version.is_none() {
        r.version = info.version.clone();
    }
    if r.md5.is_none() {
        r.md5 = info.md5.clone();
    }
    if r.size_formatted.is_none() {
        r.size_formatted = info.size.as_ref().map(formatting::format_size_value);
    }
    if r.devicename.is_none() {
        r.devicename = info.devicename.clone();
    }
    if r.maintainer.is_none() {
        r.maintainer = info.maintainer.clone();
    }
    if r.security_patch.is_none() {
        r.security_patch = info.security_patch.clone();
    }
}

fn build_resolved(
    branch: &str,
    gms: Option<&OldDeviceInfo>,
    vanilla: Option<&OldDeviceInfo>,
) -> ResolvedUpdates {
    let mut r = ResolvedUpdates {
        android_version: branch.to_string(),
        gms_url: gms.and_then(|g| g.url.clone()),
        vanilla_url: vanilla.and_then(|v| v.url.clone()),
        ..Default::default()
    };
    // GMS takes priority for shared fields, then vanilla.
    for info in [gms, vanilla].into_iter().flatten() {
        merge_info(&mut r, info);
    }
    r
}

/// Look for a device on one branch. Returns `(result, saw_failure)`.
async fn fetch_branch_update(
    client: &reqwest::Client,
    branch: &str,
    names: &[String],
) -> (Option<ResolvedUpdates>, bool) {
    let mut failed = false;

    for name in names {
        // A. gms and vanilla subdirectories, concurrently
        let gms_url = raw_url(branch, &format!("devices/gms/{name}.json"));
        let vanilla_url = raw_url(branch, &format!("devices/vanilla/{name}.json"));
        let (gms, vanilla) = tokio::join!(
            fetch_single_update_json(client, &gms_url),
            fetch_single_update_json(client, &vanilla_url)
        );

        failed |= matches!(gms, Lookup::Failed) || matches!(vanilla, Lookup::Failed);
        let gms_info = match gms {
            Lookup::Found(i) => Some(i),
            _ => None,
        };
        let vanilla_info = match vanilla {
            Lookup::Found(i) => Some(i),
            _ => None,
        };

        if gms_info.is_some() || vanilla_info.is_some() {
            let resolved = build_resolved(branch, gms_info.as_deref(), vanilla_info.as_deref());
            log::debug!(
                "Found update for {} on branch {}: GMS={}, Vanilla={}",
                name,
                branch,
                resolved.gms_url.is_some(),
                resolved.vanilla_url.is_some()
            );
            return (Some(resolved), failed);
        }

        // B. Fallback: legacy root devices/{name}.json
        match fetch_single_update_json(client, &raw_url(branch, &format!("devices/{name}.json"))).await {
            Lookup::Found(info) => {
                log::debug!("Found update in legacy directory for {} on branch {}", name, branch);
                return (Some(build_resolved(branch, None, None).with_legacy(&info)), failed);
            }
            Lookup::Failed => failed = true,
            Lookup::NotFound => {}
        }
    }

    (None, failed)
}

impl ResolvedUpdates {
    fn with_legacy(mut self, info: &OldDeviceInfo) -> Self {
        merge_info(&mut self, info);
        self
    }
}

/// Uncached lookup across all branches (newest first). Branches are queried concurrently.
/// Returns `(result, saw_failure)`.
async fn lookup_official_update(
    client: &reqwest::Client,
    codename: &str,
) -> (Option<ResolvedUpdates>, bool) {
    if !valid_codename(codename) {
        return (None, false);
    }
    let names = name_variants(codename);

    let results = futures::future::join_all(
        config::ALL_BRANCHES
            .iter()
            .map(|branch| fetch_branch_update(client, branch, &names)),
    )
    .await;

    let failed = results.iter().any(|(_, f)| *f);
    // ALL_BRANCHES is ordered newest-first, so the first hit wins.
    let found = results.into_iter().find_map(|(r, _)| r);
    if found.is_none() {
        log::debug!("No update entry found for {} across branches 17, 16, 15", codename);
    }
    (found, failed)
}

#[cfg(test)]
async fn fetch_official_update_info(
    client: &reqwest::Client,
    codename: &str,
) -> Option<ResolvedUpdates> {
    lookup_official_update(client, codename).await.0
}

/// Cached variant (5-minute TTL). Lookups that hit transient errors are not cached.
async fn fetch_official_update_info_cached(
    state: &AppState,
    codename: &str,
) -> Option<ResolvedUpdates> {
    let key = codename.to_lowercase();

    if let Some((at, value)) = state.update_cache.read().await.get(&key) {
        if at.elapsed() < UPDATE_CACHE_TTL {
            return value.clone();
        }
    }

    let (value, failed) = lookup_official_update(&state.http_client, codename).await;
    if !failed {
        state
            .update_cache
            .write()
            .await
            .insert(key, (Instant::now(), value.clone()));
    }
    value
}

// --- FETCH SITE DEVICE INFO (used by /flash) ---
pub async fn fetch_device_site_info(
    codename: &str,
    state: &AppState,
) -> Result<Option<SiteDeviceDetail>> {
    if !valid_codename(codename.trim()) {
        return Ok(None);
    }
    let correct_filename = resolve_codename(state, codename).await;

    let resp = state
        .http_client
        .get(site_device_json_url(&correct_filename))
        .send()
        .await?;

    if !resp.status().is_success() {
        return Ok(None);
    }

    let detail = resp
        .json::<SiteDeviceDetail>()
        .await
        .context("Failed to parse Site Device Detail")?;
    Ok(Some(detail))
}

// --- FETCH CHANGELOG (used by /changelog) ---
pub async fn fetch_device_changelog(codename: &str, state: &AppState) -> Result<Option<String>> {
    if !valid_codename(codename.trim()) {
        return Ok(None);
    }
    let correct_filename = resolve_codename(state, codename).await;

    // Prefer the changelog filename from the cached device summary.
    let cached_file = {
        let cache = state.devices_cache.read().await;
        cache
            .iter()
            .find(|d| d.codename.eq_ignore_ascii_case(&correct_filename))
            .and_then(|d| d.changelog.clone())
            .filter(|f| !f.contains('/') && !f.contains(".."))
    };

    let mut candidates: Vec<String> = Vec::new();
    candidates.extend(cached_file);
    for name in name_variants(&correct_filename) {
        let candidate = format!("changelog_{name}.txt");
        if !candidates.contains(&candidate) {
            candidates.push(candidate);
        }
    }

    for branch in config::ALL_BRANCHES {
        for candidate in &candidates {
            let url = raw_url(branch, &format!("{}/{}", config::DIR_CHANGELOGS, candidate));
            if let Ok(resp) = state.http_client.get(&url).send().await {
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
    if !valid_codename(codename.trim()) {
        return Ok(None);
    }
    let correct_filename = resolve_codename(state, codename).await;

    // 1. Cached DeviceSummary
    let cached_summary = {
        let cache = state.devices_cache.read().await;
        cache
            .iter()
            .find(|d| d.codename.eq_ignore_ascii_case(&correct_filename))
            .cloned()
    };

    // 2. Update info across branches 17, 16, 15 (cached)
    let updates = fetch_official_update_info_cached(state, &correct_filename).await;

    // 3. Fall back to the site detail if the device isn't in the catalog
    let site_detail = if cached_summary.is_none() {
        match fetch_device_site_info(&correct_filename, state).await {
            Ok(detail) => detail,
            Err(e) => {
                log::warn!("Site detail lookup for {} failed: {:#}", correct_filename, e);
                None
            }
        }
    } else {
        None
    };

    // Device doesn't exist in cache, site, or updates
    if cached_summary.is_none() && updates.is_none() && site_detail.is_none() {
        return Ok(None);
    }

    let device_name = cached_summary
        .as_ref()
        .map(|s| s.name.clone())
        .or_else(|| site_detail.as_ref().map(|d| d.name.clone()))
        .or_else(|| updates.as_ref().and_then(|u| u.devicename.clone()))
        .unwrap_or_else(|| correct_filename.clone());

    let maintainer_info = cached_summary
        .as_ref()
        .and_then(|s| s.maintainer.as_ref())
        .or_else(|| site_detail.as_ref().and_then(|d| d.maintainer.as_ref()));
    let maintainer = maintainer_info
        .map(|m| m.name.clone())
        .or_else(|| updates.as_ref().and_then(|u| u.maintainer.clone()))
        .unwrap_or_else(|| "Unknown".to_string());
    let maintainer_github = maintainer_info.and_then(|m| m.github_url.clone());

    let raw_version = updates
        .as_ref()
        .and_then(|u| u.version.clone())
        .or_else(|| cached_summary.as_ref().and_then(|s| s.version.clone()));
    let version_display = match raw_version {
        Some(ref v) if v.starts_with('v') => v.clone(),
        Some(ref v) => format!("v{}", v),
        None => "Upcoming".to_string(),
    };

    let android_version = updates
        .as_ref()
        .map(|u| u.android_version.clone())
        .unwrap_or_else(|| "N/A".to_string());

    let build_ts = updates
        .as_ref()
        .and_then(|u| u.latest_datetime)
        .or_else(|| cached_summary.as_ref().and_then(|s| s.datetime));
    let date_str = build_ts
        .map(formatting::format_timestamp)
        .unwrap_or_else(|| "N/A".to_string());

    let size_str = updates
        .as_ref()
        .and_then(|u| u.size_formatted.clone())
        .unwrap_or_else(|| "N/A".to_string());

    // Download URLs: ONLY populated for active Android 17 builds.
    // Builds below A17 have dead links and devices without an update entry have no build.
    let (gms_download_url, vanilla_download_url) = if android_version == config::BRANCH_A17 {
        (
            updates.as_ref().and_then(|u| u.gms_url.clone()),
            updates.as_ref().and_then(|u| u.vanilla_url.clone()),
        )
    } else {
        (None, None)
    };

    let md5 = updates
        .as_ref()
        .and_then(|u| u.md5.clone())
        .unwrap_or_else(|| "N/A".to_string());

    // Status: custom override > unreleased check > active
    let status_text = state
        .device_statuses
        .read()
        .await
        .get(&correct_filename.to_lowercase())
        .cloned()
        .unwrap_or_else(|| {
            if cached_summary.as_ref().and_then(|s| s.released) == Some(false) {
                "Upcoming".to_string()
            } else {
                "Active".to_string()
            }
        });

    // Security patch: explicit value if published, otherwise the build month
    // (clearly marked as an estimate, since it is not the real patch level).
    let security_patch = updates
        .as_ref()
        .and_then(|u| u.security_patch.clone())
        .or_else(|| site_detail.as_ref().and_then(|d| d.security_patch.clone()))
        .or_else(|| {
            build_ts
                .and_then(formatting::format_month_year)
                .map(|m| format!("{m} (est.)"))
        })
        .unwrap_or_else(|| "N/A".to_string());

    Ok(Some(UnifiedDevice {
        device_name,
        codename: correct_filename,
        maintainer,
        maintainer_github,
        version: version_display,
        android_version,
        status: status_text,
        date: date_str,
        size: size_str,
        gms_download_url,
        vanilla_download_url,
        security_patch,
        md5,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::Config, helpers::logs::LogBuffer};
    use std::collections::HashMap;

    fn info(json: &str) -> OldDeviceInfo {
        serde_json::from_str(json).expect("valid test JSON")
    }

    fn test_state() -> AppState {
        AppState::new(Config::default(), HashMap::new(), LogBuffer::new(100))
    }

    // ---------- offline unit tests ----------

    #[test]
    fn urls_are_built_from_config() {
        assert_eq!(
            site_device_json_url("peridot"),
            "https://raw.githubusercontent.com/The-Clover-Project/official_updates/17/site/devices/peridot.json"
        );
        assert!(raw_url("16", "devices/gms/x.json").ends_with("/official_updates/16/devices/gms/x.json"));
    }

    #[test]
    fn name_variants_cover_both_casings() {
        assert_eq!(name_variants("Peridot"), vec!["peridot", "Peridot"]);
        assert_eq!(name_variants("123"), vec!["123"]);
    }

    #[test]
    fn gms_wins_shared_fields_and_dates_take_the_max() {
        let gms = info(
            r#"{"datetime":200,"version":"4.0","md5":"g","size":1048576,"url":"https://x/gms.zip","maintainer":"A"}"#,
        );
        let vanilla = info(
            r#"{"datetime":300,"version":"3.9","md5":"v","size":"2 GB","url":"https://x/van.zip","devicename":"Phone","securityPatch":"2026-10-05"}"#,
        );
        let r = build_resolved("17", Some(&gms), Some(&vanilla));
        assert_eq!(r.android_version, "17");
        assert_eq!(r.latest_datetime, Some(300));
        assert_eq!(r.version.as_deref(), Some("4.0"));
        assert_eq!(r.md5.as_deref(), Some("g"));
        assert_eq!(r.size_formatted.as_deref(), Some("1.00 MB"));
        assert_eq!(r.maintainer.as_deref(), Some("A"));
        assert_eq!(r.devicename.as_deref(), Some("Phone")); // filled from vanilla
        assert_eq!(r.security_patch.as_deref(), Some("2026-10-05"));
        assert_eq!(r.gms_url.as_deref(), Some("https://x/gms.zip"));
        assert_eq!(r.vanilla_url.as_deref(), Some("https://x/van.zip"));
    }

    #[test]
    fn legacy_entries_have_no_variant_urls() {
        let legacy = info(r#"{"datetime":10,"version":"3.1","url":"https://x/a.zip"}"#);
        let r = build_resolved("16", None, None).with_legacy(&legacy);
        assert_eq!(r.latest_datetime, Some(10));
        assert_eq!(r.version.as_deref(), Some("3.1"));
        assert!(r.gms_url.is_none() && r.vanilla_url.is_none());
    }

    #[tokio::test]
    async fn invalid_codenames_never_reach_the_network() {
        let state = test_state();
        assert!(load_device_from_github("../../x", &state).await.unwrap().is_none());
        assert!(fetch_device_site_info("a b", &state).await.unwrap().is_none());
        assert!(fetch_device_changelog("x?y", &state).await.unwrap().is_none());
        assert!(fetch_official_update_info(&state.http_client, "../x").await.is_none());
    }

    #[tokio::test]
    async fn codename_lookup_is_case_insensitive() {
        let state = test_state();
        state
            .actual_filenames
            .write()
            .await
            .insert("peridot".into(), "Peridot".into());
        assert_eq!(resolve_codename(&state, "  PERIDOT ").await, "Peridot");
        assert_eq!(resolve_codename(&state, "unknown").await, "unknown");
    }

    // ---------- live-network tests (run with `cargo test -- --ignored`) ----------

    #[tokio::test]
    #[ignore = "hits the live GitHub network"]
    async fn live_preload_and_lookup() {
        let state = test_state();
        let count = preload_devices(&state).await.expect("preload should succeed");
        assert!(count >= 1, "expected at least one device");

        let first = state.devices_cache.read().await[0].codename.clone();
        let dev = load_device_from_github(&first, &state)
            .await
            .expect("load should succeed")
            .expect("device from the catalog should resolve");
        assert!(dev.codename.eq_ignore_ascii_case(&first));
        assert!(!dev.device_name.is_empty());

        let log = fetch_device_changelog(&first, &state).await.expect("changelog fetch");
        println!("changelog for {first}: {}", log.is_some());
    }

    #[tokio::test]
    #[ignore = "hits the live GitHub network"]
    async fn live_unknown_device_is_none() {
        let state = test_state();
        let _ = preload_devices(&state).await;
        let dev = load_device_from_github("definitely-not-a-device", &state)
            .await
            .expect("load should not error");
        assert!(dev.is_none());
    }
}
