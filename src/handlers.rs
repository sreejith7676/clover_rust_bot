use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use reqwest::Url;
use sysinfo::{Disks, System};
use teloxide::{
    prelude::*,
    types::{
        CallbackQuery, InlineKeyboardButton, InlineKeyboardMarkup, InputFile, ParseMode,
        ReplyParameters,
    },
    utils::html::escape,
};

use crate::{
    config,
    helpers::{
        self, formatting, github, storage,
        telegram::{delete_quiet, reply_html, sender_id},
        text::{append_limited, escape_attr, safe_link, tail_lines_within, valid_codename},
    },
    models::{DeviceSummary, InstallationStep, UnifiedDevice},
    state::AppState,
    Command,
};

type HandlerResult = Result<(), teloxide::RequestError>;

// Image asset embedded into binary
const BANNER_BYTES: &[u8] = include_bytes!("../assets/banner.png");

/// Telegram's limit is 4096 characters; stay well below it (byte count, HTML included).
const MESSAGE_BUDGET: usize = 3800;

/// Minimum gap between network-heavy commands for non-admin users.
const FETCH_COOLDOWN: Duration = Duration::from_secs(3);

const INVALID_CODENAME: &str =
    "❌ Invalid codename. Use only letters, digits, <code>_</code> or <code>-</code> (max 32 characters).";

pub async fn command_handler(
    bot: Bot,
    msg: Message,
    cmd: Command,
    state: Arc<AppState>,
) -> HandlerResult {
    let user_id = sender_id(&msg);
    let username = msg
        .from.as_ref()
        .and_then(|u| u.username.as_deref())
        .unwrap_or("Unknown");

    log::info!("Command `{:?}` from user `{}` ({})", cmd, username, user_id);

    if !state.config.is_admin(user_id) && !state.config.is_allowed(msg.chat.id) {
        reply_html(
            &bot,
            &msg,
            format!(
                "❌ <b>This bot only works in The Clover Project group!</b>\n\n📢 <b>Join our group to use this bot:</b>\n{}",
                config::GROUP_LINK
            ),
        )
        .await?;
        return Ok(());
    }

    helpers::stats::track_user(user_id, &state).await;

    // Server-info commands are limited to admins and the special group.
    if matches!(
        cmd,
        Command::Uptime | Command::Ping | Command::Stats | Command::Server
    ) && !state.config.is_special(msg.chat.id, user_id)
    {
        reply_html(&bot, &msg, "❌ This command is not available in this chat.").await?;
        return Ok(());
    }

    // Per-user cooldown for commands that hit GitHub.
    if matches!(
        cmd,
        Command::Clover(_) | Command::Changelog(_) | Command::Flash(_) | Command::Install(_)
    ) && !state.config.is_admin(user_id)
        && !state.check_cooldown(user_id, FETCH_COOLDOWN)
    {
        reply_html(&bot, &msg, "⏳ Please wait a few seconds before the next request.").await?;
        return Ok(());
    }

    match cmd {
        Command::Start => start(bot, msg, state).await?,
        Command::Clover(args) => clover_info(bot, msg, args, state).await?,
        Command::Changelog(args) => changelog_cmd(bot, msg, args, state).await?,
        Command::Flash(args) | Command::Install(args) => flash_cmd(bot, msg, args, state).await?,
        Command::Maintainer(args) => maintainer_cmd(bot, msg, args, state).await?,
        Command::List => list_devices(bot, msg, state).await?,
        Command::Maintainership => maintainership(bot, msg).await?,
        Command::Uptime => uptime(bot, msg, state).await?,
        Command::Ping => ping(bot, msg).await?,
        Command::Stats => stats_command(bot, msg, state).await?,
        Command::Server => server(bot, msg).await?,
        Command::Sync | Command::Reload => sync_cmd(bot, msg, state).await?,
        Command::Reboot => reboot(bot, msg, state).await?,
        Command::Log(args) => log_cmd(bot, msg, args, state).await?,
        Command::Clearlogs => clearlogs(bot, msg, state).await?,
    };

    Ok(())
}

/// Replies with an error and returns `false` unless the sender is an admin.
async fn require_admin(bot: &Bot, msg: &Message, state: &AppState) -> Result<bool, teloxide::RequestError> {
    if state.config.is_admin(sender_id(msg)) {
        return Ok(true);
    }
    reply_html(bot, msg, "❌ This command is only available for administrators.").await?;
    Ok(false)
}

async fn start(bot: Bot, msg: Message, state: Arc<AppState>) -> HandlerResult {
    let sender = sender_id(&msg);

    let mut welcome = "🤖 <b>Welcome to the Official Clover ROM Bot!</b>\n\n".to_string();
    welcome.push_str("📱 <b>Device Commands:</b>\n");
    welcome.push_str("• /clover &lt;codename&gt; - Get device info &amp; download links\n");
    welcome.push_str("• /changelog &lt;codename&gt; - View device build changes\n");
    welcome.push_str("• /flash &lt;codename&gt; - Step-by-step flashing guide\n");
    welcome.push_str("• /list - Interactive device catalog by manufacturer\n");
    welcome.push_str("• /maintainer [name] - View maintainers &amp; their devices\n\n");
    welcome.push_str("👥 <b>Maintainership:</b>\n");
    welcome.push_str("• /maintainership - Apply for maintainership\n");

    if state.config.is_special(msg.chat.id, sender) {
        welcome.push_str("\n⚙️ <b>Server Commands:</b>\n");
        welcome.push_str("• /uptime - Bot uptime\n");
        welcome.push_str("• /ping - Bot response time\n");
        welcome.push_str("• /stats - Bot usage stats\n");
        welcome.push_str("• /server - Server details\n");
    }

    if state.config.is_admin(sender) {
        welcome.push_str("\n🔧 <b>Admin Commands:</b>\n");
        welcome.push_str("• /sync - Resync device catalog from GitHub\n");
        welcome.push_str("• /reboot - Restart the bot\n");
        welcome.push_str("• /log [lines|file] - View or download bot logs\n");
        welcome.push_str("• /clearlogs - Clear bot logs\n");
        welcome.push_str("• /clover insert &lt;device&gt; &lt;status&gt; - Set status\n");
        welcome.push_str("• /clover remove &lt;device&gt; - Remove status\n");
        welcome.push_str("• /clover inactive - List custom statuses\n");
    }

    reply_html(&bot, &msg, welcome).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// /clover
// ---------------------------------------------------------------------------

async fn clover_info(bot: Bot, msg: Message, raw_args: String, state: Arc<AppState>) -> HandlerResult {
    let trimmed_args = raw_args.trim();
    let args: Vec<&str> = trimmed_args.split_whitespace().collect();
    let sub = args.first().map(|s| s.to_lowercase()).unwrap_or_default();

    match sub.as_str() {
        "insert" => return clover_insert(&bot, &msg, &args, &state).await,
        "remove" => return clover_remove(&bot, &msg, &args, &state).await,
        "inactive" => return clover_inactive(&bot, &msg, &state).await,
        _ => {}
    }

    let codename = trimmed_args;
    if codename.is_empty() {
        reply_html(
            &bot,
            &msg,
            "❌ Please specify a device codename.\nExample: <code>/clover peridot</code>",
        )
        .await?;
        return Ok(());
    }
    if !valid_codename(codename) {
        reply_html(&bot, &msg, INVALID_CODENAME).await?;
        return Ok(());
    }

    let status_msg = reply_html(&bot, &msg, format!("🔍 <b>Fetching '{}'...</b>", escape(codename))).await?;

    let outcome = match github::load_device_from_github(codename, &state).await {
        Ok(Some(device)) => {
            state.stats.write().await.devices_queried += 1;
            send_device_card(&bot, &msg, &state, &device).await
        }
        Ok(None) => reply_html(
            &bot,
            &msg,
            format!(
                "❌ No information for <code>{}</code>\nUse /list to see available devices.",
                escape(codename)
            ),
        )
        .await
        .map(|_| ()),
        Err(e) => {
            log::error!("Error fetching device info for {}: {:#}", codename, e);
            reply_html(&bot, &msg, "❌ An error occurred while fetching device data.")
                .await
                .map(|_| ())
        }
    };

    // Always remove the placeholder, even if sending the result failed.
    delete_quiet(&bot, msg.chat.id, status_msg.id).await;
    outcome
}

async fn clover_insert(bot: &Bot, msg: &Message, args: &[&str], state: &AppState) -> HandlerResult {
    if !require_admin(bot, msg, state).await? {
        return Ok(());
    }
    if args.len() < 3 {
        reply_html(bot, msg, "❌ Usage: <code>/clover insert &lt;device&gt; &lt;status&gt;</code>").await?;
        return Ok(());
    }
    let codename = args[1].to_lowercase();
    if !valid_codename(&codename) {
        reply_html(bot, msg, INVALID_CODENAME).await?;
        return Ok(());
    }
    let status: String = args[2..].join(" ").replace('"', "").chars().take(64).collect();

    // Hold the lock only long enough to update and snapshot; never across I/O or network calls.
    let snapshot = {
        let mut statuses = state.device_statuses.write().await;
        statuses.insert(codename.clone(), status.clone());
        statuses.clone()
    };

    let mut text = format!(
        "✅ Set status for <b>{}</b> to <b>{}</b>",
        escape(&codename),
        escape(&status)
    );
    if let Err(e) = storage::save_statuses(&snapshot).await {
        log::error!("Failed to persist device statuses: {}", e);
        text.push_str("\n⚠️ Could not save to disk; this change will be lost on restart.");
    }
    reply_html(bot, msg, text).await?;
    Ok(())
}

async fn clover_remove(bot: &Bot, msg: &Message, args: &[&str], state: &AppState) -> HandlerResult {
    if !require_admin(bot, msg, state).await? {
        return Ok(());
    }
    if args.len() < 2 {
        reply_html(bot, msg, "❌ Usage: <code>/clover remove &lt;device&gt;</code>").await?;
        return Ok(());
    }
    let codename = args[1].to_lowercase();

    let snapshot = {
        let mut statuses = state.device_statuses.write().await;
        statuses.remove(&codename).map(|_| statuses.clone())
    };

    match snapshot {
        Some(snapshot) => {
            let mut text = format!("✅ Removed custom status for <b>{}</b>", escape(&codename));
            if let Err(e) = storage::save_statuses(&snapshot).await {
                log::error!("Failed to persist device statuses: {}", e);
                text.push_str("\n⚠️ Could not save to disk; this change will be lost on restart.");
            }
            reply_html(bot, msg, text).await?;
        }
        None => {
            reply_html(bot, msg, format!("⚠️ No custom status found for <b>{}</b>", escape(&codename))).await?;
        }
    }
    Ok(())
}

async fn clover_inactive(bot: &Bot, msg: &Message, state: &AppState) -> HandlerResult {
    let mut entries: Vec<(String, String)> = state
        .device_statuses
        .read()
        .await
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    if entries.is_empty() {
        reply_html(bot, msg, "ℹ️ No devices are manually marked as Inactive.").await?;
        return Ok(());
    }
    entries.sort();

    let mut text = "🚫 <b>Inactive / Custom Status Devices:</b>\n\n".to_string();
    for (code, stat) in &entries {
        let line = format!("• <b>{}</b>: {}\n", escape(code), escape(stat));
        if !append_limited(&mut text, &line, MESSAGE_BUDGET) {
            text.push_str("…\n");
            break;
        }
    }
    reply_html(bot, msg, text).await?;
    Ok(())
}

fn website_url(codename: &str) -> Url {
    Url::parse(&format!("{}/download/{}", config::WEBSITE_URL, codename))
        .unwrap_or_else(|_| Url::parse(config::WEBSITE_URL).expect("valid constant URL"))
}

fn device_caption(d: &UnifiedDevice) -> String {
    let status_emoji = if d.status == "Active" || d.status == "Stable" { "🟢" } else { "🔴" };

    let maintainer_display = match d.maintainer_github.as_deref().and_then(safe_link) {
        Some(url) => format!("<a href=\"{}\">{}</a>", escape_attr(&url), escape(&d.maintainer)),
        None => escape(&d.maintainer),
    };

    let version_line = if d.android_version == "N/A" || d.android_version.is_empty() {
        format!("<b>Version:</b> {}\n", escape(&d.version))
    } else {
        format!(
            "<b>Version:</b> {} (Android {})\n",
            escape(&d.version),
            escape(&d.android_version)
        )
    };

    let mut caption = String::new();
    caption.push_str("📱 <b>Device</b>\n");
    caption.push_str(&format!("<b>{}</b> — <code>{}</code>\n\n", escape(&d.device_name), escape(&d.codename)));
    caption.push_str("🛠 <b>Build Info</b>\n");
    caption.push_str(&format!("<b>Status:</b> {} {}\n", status_emoji, escape(&d.status)));
    caption.push_str(&version_line);
    caption.push_str(&format!("<b>Date:</b> {}\n", escape(&d.date)));
    caption.push_str(&format!("<b>File Size:</b> {}\n\n", escape(&d.size)));
    caption.push_str("🛡 <b>Security</b>\n");
    caption.push_str(&format!("<b>Patch Level:</b> {}\n", escape(&d.security_patch)));
    caption.push_str(&format!("<b>MD5:</b> <code>{}</code>\n\n", escape(&d.md5)));
    caption.push_str(&format!("🧑‍💻 <b>Maintainer:</b> {}\n\n", maintainer_display));
    caption.push_str(&format!(
        "💡 <i>Commands: <code>/flash {c}</code> • <code>/changelog {c}</code></i>",
        c = escape(&d.codename)
    ));
    caption
}

/// Download buttons are only shown for Android 17 builds with valid URLs
/// (older builds have dead links; devices without an update entry have no build).
fn device_keyboard(d: &UnifiedDevice) -> InlineKeyboardMarkup {
    let mut rows: Vec<Vec<InlineKeyboardButton>> = Vec::new();

    if d.android_version == config::BRANCH_A17 {
        let mut row = Vec::new();
        if let Some(url) = d.gms_download_url.as_deref().and_then(|u| Url::parse(u).ok()) {
            row.push(InlineKeyboardButton::url("🧩 GMS", url));
        }
        if let Some(url) = d.vanilla_download_url.as_deref().and_then(|u| Url::parse(u).ok()) {
            row.push(InlineKeyboardButton::url("🍦Vanilla", url));
        }
        if !row.is_empty() {
            rows.push(row);
        }
    }

    rows.push(vec![InlineKeyboardButton::url("🌐 Website", website_url(&d.codename))]);
    InlineKeyboardMarkup::new(rows)
}

async fn send_device_card(bot: &Bot, msg: &Message, state: &AppState, device: &UnifiedDevice) -> HandlerResult {
    let caption = device_caption(device);
    let keyboard = device_keyboard(device);

    // Reuse the Telegram file_id of the banner after the first upload.
    let cached = state.banner_file_id.read().await.clone();
    if let Some(file_id) = cached {
        let res = bot
            .send_photo(msg.chat.id, InputFile::file_id(file_id))
            .caption(caption.clone())
            .parse_mode(ParseMode::Html)
            .reply_parameters(ReplyParameters::new(msg.id))
            .reply_markup(keyboard.clone())
            .await;
        match res {
            Ok(_) => return Ok(()),
            Err(e) => {
                log::warn!("Cached banner file_id failed ({}); re-uploading the banner.", e);
                *state.banner_file_id.write().await = None;
            }
        }
    }

    let sent = bot
        .send_photo(msg.chat.id, InputFile::memory(BANNER_BYTES).file_name("banner.png"))
        .caption(caption)
        .parse_mode(ParseMode::Html)
        .reply_parameters(ReplyParameters::new(msg.id))
        .reply_markup(keyboard)
        .await?;

    if let Some(largest) = sent.photo().and_then(|photos| photos.last()) {
        *state.banner_file_id.write().await = Some(largest.file.id.clone());
        log::info!("Cached Telegram file_id for banner image: {}", largest.file.id);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// /changelog
// ---------------------------------------------------------------------------

/// Formats the changelog (latest two versions) as Telegram HTML.
/// Output is built from whole, self-contained pieces, so truncation at `limit`
/// can never leave a dangling tag. Returns `(html, truncated)`.
fn format_changelog_text(raw: &str, limit: usize) -> (String, bool) {
    let mut out = String::new();
    let mut version_count = 0;
    let mut truncated = false;

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if !out.is_empty() && !out.ends_with("\n\n") && !append_limited(&mut out, "\n", limit) {
                truncated = true;
                break;
            }
            continue;
        }

        let piece = if trimmed.starts_with("=====") {
            if version_count >= 2 {
                break;
            }
            version_count += 1;
            format!("📌 <b>{}</b>\n", escape(trimmed.trim_matches('=').trim()))
        } else if trimmed.starts_with('*') || trimmed.starts_with('•') {
            let item = trimmed.trim_start_matches('*').trim_start_matches('•').trim();
            format!("• {}\n", escape(item))
        } else if ["Highlights &", "Build type:", "Device:", "Device maintainer:", "Credits:"]
            .iter()
            .any(|p| trimmed.starts_with(p))
        {
            format!("<i>{}</i>\n", escape(trimmed))
        } else {
            format!("{}\n", escape(trimmed))
        };

        if !append_limited(&mut out, &piece, limit) {
            truncated = true;
            break;
        }
    }

    if out.trim().is_empty() {
        // Unstructured changelog: show it as plain escaped text.
        out.clear();
        for line in raw.lines() {
            if !append_limited(&mut out, &format!("{}\n", escape(line)), limit) {
                truncated = true;
                break;
            }
        }
    }

    (out, truncated)
}

async fn changelog_cmd(bot: Bot, msg: Message, raw_args: String, state: Arc<AppState>) -> HandlerResult {
    let codename = raw_args.trim();
    if codename.is_empty() {
        reply_html(
            &bot,
            &msg,
            "❌ Please specify a device codename.\nExample: <code>/changelog peridot</code>",
        )
        .await?;
        return Ok(());
    }
    if !valid_codename(codename) {
        reply_html(&bot, &msg, INVALID_CODENAME).await?;
        return Ok(());
    }

    let status_msg = reply_html(
        &bot,
        &msg,
        format!("🔍 <b>Fetching changelog for '{}'...</b>", escape(codename)),
    )
    .await?;

    let changelog_res = github::fetch_device_changelog(codename, &state).await;
    delete_quiet(&bot, msg.chat.id, status_msg.id).await;

    match changelog_res {
        Ok(Some(raw_text)) => {
            let display_name = {
                let cache = state.devices_cache.read().await;
                cache
                    .iter()
                    .find(|d| d.codename.eq_ignore_ascii_case(codename))
                    .map(|d| d.name.clone())
                    .unwrap_or_else(|| codename.to_string())
            };

            let mut text = format!(
                "📜 <b>Changelog — {}</b> (<code>{}</code>)\n━━━━━━━━━━━━━━━━━━━━━━\n\n",
                escape(&display_name),
                escape(codename)
            );
            let (body, truncated) = format_changelog_text(&raw_text, MESSAGE_BUDGET.saturating_sub(text.len() + 100));
            text.push_str(&body);
            if truncated {
                text.push_str("...\n\n<i>(Changelog truncated. View full changelog on website.)</i>");
            }

            let keyboard = InlineKeyboardMarkup::new(vec![vec![InlineKeyboardButton::url(
                "🌐 View on Website",
                website_url(codename),
            )]]);

            reply_html(&bot, &msg, text).reply_markup(keyboard).await?;
        }
        Ok(None) => {
            reply_html(
                &bot,
                &msg,
                format!(
                    "ℹ️ No specific changelog found for <code>{}</code>. Use /list to see supported devices.",
                    escape(codename)
                ),
            )
            .await?;
        }
        Err(e) => {
            log::error!("Error fetching changelog for {}: {:#}", codename, e);
            reply_html(&bot, &msg, "❌ An error occurred while fetching device changelog.").await?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// /flash
// ---------------------------------------------------------------------------

/// Escapes a guide step, keeping `<code>` / `<pre><code>` blocks — but only when
/// they are balanced; otherwise everything is escaped so Telegram can't reject the message.
fn sanitize_guide_html(text: &str) -> String {
    let balanced = text.matches("<code>").count() == text.matches("</code>").count()
        && text.matches("<pre>").count() == text.matches("</pre>").count();
    if !balanced {
        return escape(text);
    }

    let p1 = text
        .replace("<pre><code>", "___PRE_CODE___")
        .replace("</code></pre>", "___END_PRE_CODE___")
        .replace("<code>", "___CODE___")
        .replace("</code>", "___END_CODE___");
    escape(&p1)
        .replace("___PRE_CODE___", "<pre><code>")
        .replace("___END_PRE_CODE___", "</code></pre>")
        .replace("___CODE___", "<code>")
        .replace("___END_CODE___", "</code>")
}

fn build_guide_text(name: &str, codename: &str, steps: &[InstallationStep]) -> String {
    let mut text = format!(
        "📖 <b>Flashing Guide — {}</b> (<code>{}</code>)\n━━━━━━━━━━━━━━━━━━━━━━\n\n",
        escape(name),
        escape(codename)
    );

    let mut truncated = false;
    for (idx, step) in steps.iter().enumerate() {
        let piece = format!(
            "<b>{}. {}</b>\n{}\n\n",
            idx + 1,
            escape(&step.title),
            sanitize_guide_html(&step.description)
        );
        if !append_limited(&mut text, &piece, MESSAGE_BUDGET) {
            truncated = true;
            break;
        }
    }
    if truncated {
        text.push_str("...\n\n<i>(Guide truncated. View full guide on website.)</i>");
    }
    text
}

async fn flash_cmd(bot: Bot, msg: Message, raw_args: String, state: Arc<AppState>) -> HandlerResult {
    let codename = raw_args.trim();
    if codename.is_empty() {
        reply_html(
            &bot,
            &msg,
            "❌ Please specify a device codename.\nExample: <code>/flash peridot</code>",
        )
        .await?;
        return Ok(());
    }
    if !valid_codename(codename) {
        reply_html(&bot, &msg, INVALID_CODENAME).await?;
        return Ok(());
    }

    let status_msg = reply_html(
        &bot,
        &msg,
        format!("🔍 <b>Fetching flashing guide for '{}'...</b>", escape(codename)),
    )
    .await?;

    let site_info = github::fetch_device_site_info(codename, &state).await;
    delete_quiet(&bot, msg.chat.id, status_msg.id).await;

    match site_info {
        Ok(Some(root)) => {
            let steps = root.installation_guide.map(|g| g.steps).unwrap_or_default();
            if steps.is_empty() {
                reply_html(
                    &bot,
                    &msg,
                    format!(
                        "ℹ️ No specific flashing guide found for <b>{}</b> (<code>{}</code>).",
                        escape(&root.name),
                        escape(codename)
                    ),
                )
                .await?;
                return Ok(());
            }

            let text = build_guide_text(&root.name, codename, &steps);
            let keyboard = InlineKeyboardMarkup::new(vec![vec![InlineKeyboardButton::url(
                "🌐 Web Guide & Downloads",
                website_url(codename),
            )]]);
            reply_html(&bot, &msg, text).reply_markup(keyboard).await?;
        }
        Ok(None) => {
            reply_html(
                &bot,
                &msg,
                format!("❌ Device <code>{}</code> not found. Use /list to see supported devices.", escape(codename)),
            )
            .await?;
        }
        Err(e) => {
            log::error!("Error fetching flashing guide for {}: {:#}", codename, e);
            reply_html(&bot, &msg, "❌ An error occurred while fetching flashing guide.").await?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// /maintainer
// ---------------------------------------------------------------------------

/// Builds the /maintainer reply. With an empty query: overview of all maintainers.
/// Otherwise: the profile of the first matching maintainer and only *their* devices.
fn maintainer_text(devices: &[DeviceSummary], query: &str) -> String {
    let query = query.trim().to_lowercase();

    if query.is_empty() {
        let mut by_maintainer: BTreeMap<String, Vec<&str>> = BTreeMap::new();
        for dev in devices {
            let name = dev.maintainer.as_ref().map(|m| m.name.as_str()).unwrap_or("Unknown");
            by_maintainer.entry(name.to_string()).or_default().push(dev.codename.as_str());
        }

        let mut text = "👥 <b>The Clover Project Maintainers</b>\n━━━━━━━━━━━━━━━━━━━━━━\n\n".to_string();
        let mut omitted = 0usize;
        let mut full = false;
        for (maintainer, codenames) in &by_maintainer {
            let line = format!("• <b>{}</b> — {}\n", escape(maintainer), escape(&codenames.join(", ")));
            if full || !append_limited(&mut text, &line, MESSAGE_BUDGET - 200) {
                full = true;
                omitted += 1;
            }
        }
        if omitted > 0 {
            text.push_str(&format!("…and {omitted} more maintainers.\n"));
        }
        text.push_str("\n<i>💡 Tip: Use <code>/maintainer &lt;name&gt;</code> for details.</i>");
        return text;
    }

    let matches: Vec<&DeviceSummary> = devices
        .iter()
        .filter(|dev| {
            dev.maintainer.as_ref().is_some_and(|m| {
                m.name.to_lowercase().contains(&query)
                    || m.github_url.as_ref().is_some_and(|u| u.to_lowercase().contains(&query))
                    || dev.codename.to_lowercase() == query
            })
        })
        .collect();

    // Profile belongs to the first match; only list devices of that same maintainer.
    let Some(first) = matches.first().and_then(|d| d.maintainer.as_ref()) else {
        return format!(
            "❌ No maintainer found matching '<b>{}</b>'. Use <code>/maintainer</code> to see all maintainers.",
            escape(&query)
        );
    };

    let github_link = match first.github_url.as_deref().and_then(safe_link) {
        Some(url) => format!("<a href=\"{}\">{}</a>", escape_attr(&url), escape(&url)),
        None => "N/A".to_string(),
    };

    let mut text = format!(
        "🧑‍💻 <b>Maintainer Profile</b>\n━━━━━━━━━━━━━━━━━━━━━━\n👤 <b>Name:</b> {}\n🔗 <b>GitHub:</b> {}\n\n📱 <b>Maintained Devices:</b>\n",
        escape(&first.name),
        github_link
    );

    for dev in matches
        .iter()
        .filter(|d| d.maintainer.as_ref().is_some_and(|m| m.name == first.name))
    {
        let line = format!("• {} (<code>{}</code>)\n", escape(&dev.name), escape(&dev.codename));
        if !append_limited(&mut text, &line, MESSAGE_BUDGET) {
            text.push_str("…\n");
            break;
        }
    }
    text
}

async fn maintainer_cmd(bot: Bot, msg: Message, raw_args: String, state: Arc<AppState>) -> HandlerResult {
    // Build the text while holding the lock, send after releasing it.
    let text = {
        let devices = state.devices_cache.read().await;
        if devices.is_empty() {
            None
        } else {
            Some(maintainer_text(&devices, &raw_args))
        }
    };

    match text {
        Some(text) => reply_html(&bot, &msg, text).await?,
        None => reply_html(&bot, &msg, "❌ Device list is loading... Please try again in a moment.").await?,
    };
    Ok(())
}

// ---------------------------------------------------------------------------
// /list & callbacks
// ---------------------------------------------------------------------------

fn normalize_brand(raw_brand: &str) -> String {
    let lower = raw_brand.to_lowercase();
    if lower.contains("xiaomi")
        || lower.contains("redmi")
        || lower.contains("poco")
        || lower.split_whitespace().any(|w| w == "mi")
    {
        "Xiaomi / Redmi / Poco".to_string()
    } else if lower.contains("motorola") {
        "Motorola".to_string()
    } else if lower.contains("realme") {
        "Realme".to_string()
    } else if lower.contains("nothing") {
        "Nothing".to_string()
    } else {
        raw_brand.to_string()
    }
}

fn get_sorted_brands(devices: &[DeviceSummary]) -> Vec<String> {
    let mut brands: Vec<String> = devices
        .iter()
        .map(|d| normalize_brand(&d.brand))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();

    let priority = |b: &str| -> i32 {
        if b.contains("Xiaomi") {
            0
        } else if b.contains("Motorola") {
            1
        } else if b.contains("Realme") {
            2
        } else if b.contains("Nothing") {
            3
        } else {
            4
        }
    };

    brands.sort_by(|a, b| priority(a).cmp(&priority(b)).then_with(|| a.cmp(b)));
    brands
}

fn make_brand_menu_keyboard(brands: &[String]) -> InlineKeyboardMarkup {
    let mut rows: Vec<Vec<InlineKeyboardButton>> = Vec::new();
    let mut current_row: Vec<InlineKeyboardButton> = Vec::new();

    for brand in brands {
        current_row.push(InlineKeyboardButton::callback(
            format!("📱 {}", brand),
            format!("brand:{}", brand),
        ));
        if current_row.len() == 2 {
            rows.push(std::mem::take(&mut current_row));
        }
    }
    if !current_row.is_empty() {
        rows.push(current_row);
    }

    rows.push(vec![InlineKeyboardButton::callback("🌐 View Full Catalog", "list:all")]);
    InlineKeyboardMarkup::new(rows)
}

fn brand_menu_view(devices: &[DeviceSummary]) -> (String, InlineKeyboardMarkup) {
    let text = format!(
        "📋 <b>The Clover Project — Supported Devices ({} Total)</b>\n\nSelect a manufacturer below to browse devices, or view the complete catalog:",
        devices.len()
    );
    (text, make_brand_menu_keyboard(&get_sorted_brands(devices)))
}

fn full_catalog_text(devices: &[DeviceSummary]) -> String {
    let mut grouped: BTreeMap<String, Vec<&DeviceSummary>> = BTreeMap::new();
    for device in devices {
        grouped.entry(normalize_brand(&device.brand)).or_default().push(device);
    }

    let limit = MESSAGE_BUDGET - 200; // leave room for the footer
    let mut out = "📋 <b>List of Supported Devices:</b>\n\n".to_string();
    let mut omitted = 0usize;
    let mut full = false;

    for brand in get_sorted_brands(devices) {
        let Some(list) = grouped.get_mut(&brand) else { continue };
        list.sort_by(|a, b| a.name.cmp(&b.name));

        if full || !append_limited(&mut out, &format!("<b>{}</b>\n", escape(&brand)), limit) {
            full = true;
            omitted += list.len();
            continue;
        }
        for dev in list.iter() {
            let line = format!("• {} (<code>{}</code>)\n", escape(&dev.name), escape(&dev.codename));
            if full || !append_limited(&mut out, &line, limit) {
                full = true;
                omitted += 1;
            }
        }
        out.push('\n');
    }

    if omitted > 0 {
        out.push_str(&format!("…and {omitted} more. Browse by brand to see them all.\n\n"));
    }
    out.push_str("Use /clover &lt;codename&gt; to get details.");
    out
}

fn brand_view(devices: &[DeviceSummary], target_brand: &str) -> (String, InlineKeyboardMarkup) {
    let mut matching: Vec<&DeviceSummary> = devices
        .iter()
        .filter(|d| normalize_brand(&d.brand) == target_brand)
        .collect();
    matching.sort_by(|a, b| a.name.cmp(&b.name));

    let mut text = format!(
        "📱 <b>{} Devices ({} supported):</b>\n━━━━━━━━━━━━━━━━━━━━━━\n\n",
        escape(target_brand),
        matching.len()
    );
    for dev in &matching {
        let line = format!("• {} (<code>{}</code>)\n", escape(&dev.name), escape(&dev.codename));
        if !append_limited(&mut text, &line, MESSAGE_BUDGET - 120) {
            text.push_str("…\n");
            break;
        }
    }
    text.push_str("\n💡 <i>Use <code>/clover &lt;codename&gt;</code> for downloads &amp; details.</i>");

    let keyboard = InlineKeyboardMarkup::new(vec![vec![
        InlineKeyboardButton::callback("⬅️ Back to Brands", "list:menu"),
        InlineKeyboardButton::callback("🌐 Full Catalog", "list:all"),
    ]]);
    (text, keyboard)
}

fn callback_view(devices: &[DeviceSummary], data: &str) -> Option<(String, InlineKeyboardMarkup)> {
    match data {
        "list:menu" => Some(brand_menu_view(devices)),
        "list:all" => Some((
            full_catalog_text(devices),
            InlineKeyboardMarkup::new(vec![vec![InlineKeyboardButton::callback(
                "⬅️ Back to Brands",
                "list:menu",
            )]]),
        )),
        _ => data.strip_prefix("brand:").map(|brand| brand_view(devices, brand)),
    }
}

async fn list_devices(bot: Bot, msg: Message, state: Arc<AppState>) -> HandlerResult {
    let view = {
        let devices = state.devices_cache.read().await;
        if devices.is_empty() {
            None
        } else {
            Some(brand_menu_view(&devices))
        }
    };

    match view {
        Some((text, keyboard)) => {
            reply_html(&bot, &msg, text).reply_markup(keyboard).await?;
        }
        None => {
            reply_html(&bot, &msg, "❌ Device list is empty or loading... Try again in a moment.").await?;
        }
    }
    Ok(())
}

pub async fn callback_handler(bot: Bot, q: CallbackQuery, state: Arc<AppState>) -> HandlerResult {
    // Always acknowledge so the client stops showing the loading spinner.
    bot.answer_callback_query(q.id.clone()).await?;

    let (Some(data), Some(message)) = (q.data.as_deref(), q.message.as_ref()) else {
        return Ok(());
    };

    let chat_id = message.chat().id;
    if !state.config.is_admin(q.from.id) && !state.config.is_allowed(chat_id) {
        return Ok(());
    }

    let view = {
        let devices = state.devices_cache.read().await;
        callback_view(&devices, data)
    };

    if let Some((text, keyboard)) = view {
        // "message is not modified" and similar edit errors are harmless.
        if let Err(e) = bot
            .edit_message_text(chat_id, message.id(), text)
            .parse_mode(ParseMode::Html)
            .reply_markup(keyboard)
            .await
        {
            log::debug!("Could not edit catalog message: {}", e);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Misc commands
// ---------------------------------------------------------------------------

async fn maintainership(bot: Bot, msg: Message) -> HandlerResult {
    let text = "👥 <b>Maintainership Application</b>\n\n\
        Interested in maintaining Clover ROM for your device?\n\n\
        📋 <b>Requirements:</b>\n\
        • Basic knowledge of Android ROM development\n\
        • Device tree experience\n\
        • Commitment to regular updates";

    let url = Url::parse("https://github.com/The-Clover-Project/official_updates/issues/new?template=maintainer_application.yml")
        .expect("valid constant URL");
    let keyboard = InlineKeyboardMarkup::new(vec![vec![InlineKeyboardButton::url("📝 Apply Here", url)]]);

    reply_html(&bot, &msg, text).reply_markup(keyboard).await?;
    Ok(())
}

async fn uptime(bot: Bot, msg: Message, state: Arc<AppState>) -> HandlerResult {
    let formatted = formatting::format_uptime(state.bot_start_time.elapsed().as_secs());
    reply_html(&bot, &msg, format!("⏰ <b>Bot Uptime:</b> {}", formatted)).await?;
    Ok(())
}

async fn ping(bot: Bot, msg: Message) -> HandlerResult {
    let start_time = Instant::now();
    let sent = reply_html(&bot, &msg, "🏓 <b>Pinging...</b>").await?;
    let rt = start_time.elapsed().as_millis();
    bot.edit_message_text(msg.chat.id, sent.id, format!("🏓 <b>Pong!</b> Response time: {}ms", rt))
        .parse_mode(ParseMode::Html)
        .await?;
    Ok(())
}

async fn stats_command(bot: Bot, msg: Message, state: Arc<AppState>) -> HandlerResult {
    let text = {
        let stats = state.stats.read().await;
        format!(
            "📊 <b>Bot Statistics</b>\n\n• Total unique users: {}\n• Commands processed: {}\n• Devices queried: {}\n• Uptime: {}",
            stats.total_users.len(),
            stats.commands_processed,
            stats.devices_queried,
            formatting::format_uptime(state.bot_start_time.elapsed().as_secs())
        )
    };
    reply_html(&bot, &msg, text).await?;
    Ok(())
}

async fn server(bot: Bot, msg: Message) -> HandlerResult {
    // Only refresh what we report; `System::new_all()` would scan every process.
    let mut sys = System::new();
    sys.refresh_cpu_usage();
    tokio::time::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL).await;
    sys.refresh_cpu_usage();
    sys.refresh_memory();

    let disks = Disks::new_with_refreshed_list();
    // Prefer the root filesystem; the "first" disk is arbitrary (could be /boot or a tmpfs).
    let disk_info = disks
        .list()
        .iter()
        .find(|d| d.mount_point() == Path::new("/"))
        .or_else(|| disks.list().first())
        .map(|disk| {
            let total = disk.total_space();
            let used = total.saturating_sub(disk.available_space());
            let pct = if total > 0 { used as f64 / total as f64 * 100.0 } else { 0.0 };
            format!("{:.2}% ({}GB / {}GB)", pct, used / 1024 / 1024 / 1024, total / 1024 / 1024 / 1024)
        })
        .unwrap_or_else(|| "N/A".to_string());

    let ram_total = sys.total_memory();
    let ram_used = sys.used_memory();
    let ram_pct = if ram_total > 0 { ram_used as f64 / ram_total as f64 * 100.0 } else { 0.0 };

    let text = format!(
        "🖥 <b>Server Status</b>\n━━━━━━━━━━━━━━━━━━━━━━\n\
        📟 <b>System:</b> {} {}\n\
        💻 <b>CPU Usage:</b> {:.2}%\n\
        🧠 <b>RAM Usage:</b> {:.2}% ({}MB / {}MB)\n\
        💾 <b>Disk Usage:</b> {}\n\
        ⏱ <b>Uptime:</b> {}",
        escape(&System::name().unwrap_or_default()),
        escape(&System::os_version().unwrap_or_default()),
        sys.global_cpu_usage(),
        ram_pct,
        ram_used / 1024 / 1024,
        ram_total / 1024 / 1024,
        disk_info,
        formatting::format_uptime(System::uptime())
    );

    reply_html(&bot, &msg, text).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Admin commands
// ---------------------------------------------------------------------------

async fn sync_cmd(bot: Bot, msg: Message, state: Arc<AppState>) -> HandlerResult {
    if !require_admin(&bot, &msg, &state).await? {
        return Ok(());
    }
    let user_id = sender_id(&msg);

    let progress = reply_html(&bot, &msg, "🔄 <b>Syncing device catalog from GitHub...</b>").await?;
    let result = github::preload_devices(&state).await;
    delete_quiet(&bot, msg.chat.id, progress.id).await;

    match result {
        Ok(count) => {
            log::info!("Admin {} synced device catalog ({} devices)", user_id, count);
            reply_html(
                &bot,
                &msg,
                format!("✅ <b>Device catalog synced successfully!</b>\nPreloaded <b>{}</b> devices into memory cache.", count),
            )
            .await?;
        }
        Err(e) => {
            log::error!("Failed to sync device catalog: {:#}", e);
            reply_html(
                &bot,
                &msg,
                format!("❌ Failed to sync device catalog: {}", escape(&format!("{:#}", e))),
            )
            .await?;
        }
    }
    Ok(())
}

/// Graceful restart: stop polling (so Telegram gets the final update offset and the
/// /reboot command isn't replayed), flush logs, then exit with a non-zero status so a
/// supervisor using `Restart=on-failure` / `always` brings the bot back up.
async fn reboot(bot: Bot, msg: Message, state: Arc<AppState>) -> HandlerResult {
    if !require_admin(&bot, &msg, &state).await? {
        return Ok(());
    }

    reply_html(&bot, &msg, "🔄 <b>Rebooting bot...</b>").await?;
    log::warn!("Reboot command initiated by admin {}", sender_id(&msg));

    state.reboot_requested.store(true, Ordering::SeqCst);
    state.shutdown.notify_one();
    Ok(())
}

async fn log_cmd(bot: Bot, msg: Message, args: String, state: Arc<AppState>) -> HandlerResult {
    if !require_admin(&bot, &msg, &state).await? {
        return Ok(());
    }

    let trimmed = args.trim().to_lowercase();

    // Full log as a document (falls back to the in-memory buffer if there is no file).
    if matches!(trimmed.as_str(), "file" | "doc" | "download") {
        let (doc, caption) = if tokio::fs::try_exists(config::LOG_FILE).await.unwrap_or(false) {
            (InputFile::file(config::LOG_FILE), "📄 <b>Full Clover Bot Log File</b>")
        } else {
            (
                InputFile::memory(state.log_buffer.get_all_as_string().into_bytes()),
                "📄 <b>Clover Bot In-Memory Logs</b>",
            )
        };
        bot.send_document(msg.chat.id, doc.file_name("clover_bot.log"))
            .caption(caption)
            .parse_mode(ParseMode::Html)
            .reply_parameters(ReplyParameters::new(msg.id))
            .await?;
        return Ok(());
    }

    let lines = trimmed
        .split_whitespace()
        .next()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(40)
        .clamp(5, 200);

    let joined = state.log_buffer.get_last_lines(lines).join("\n");
    let text = if joined.trim().is_empty() {
        "📝 <b>Bot Logs:</b>\nLog buffer is empty or no events recorded yet.".to_string()
    } else {
        // Escaping happens inside, on char boundaries, so the result is always valid HTML.
        let (body, truncated) = tail_lines_within(&joined, 3500);
        let note = if truncated { "\n<i>(older lines omitted)</i>" } else { "" };
        format!(
            "📝 <b>Bot Logs (last {} lines):</b>\n<pre>{}</pre>{}\n<i>💡 Tip: Use <code>/log file</code> to download the full log.</i>",
            lines, body, note
        )
    };

    reply_html(&bot, &msg, text).await?;
    Ok(())
}

async fn clearlogs(bot: Bot, msg: Message, state: Arc<AppState>) -> HandlerResult {
    if !require_admin(&bot, &msg, &state).await? {
        return Ok(());
    }
    let user_id = sender_id(&msg);

    state.log_buffer.clear();

    let truncate_result = tokio::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(config::LOG_FILE)
        .await;

    match truncate_result {
        Ok(_) => {
            log::info!("Log buffer and log file cleared by admin {}", user_id);
            reply_html(&bot, &msg, "✅ <b>Logs cleared successfully!</b> Both in-memory buffer and log file were reset.").await?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            log::info!("In-memory log buffer cleared by admin {}", user_id);
            reply_html(&bot, &msg, "✅ <b>In-memory logs cleared.</b> (No log file on disk).").await?;
        }
        Err(e) => {
            log::error!("Failed to truncate log file: {}", e);
            reply_html(
                &bot,
                &msg,
                format!("⚠️ In-memory buffer cleared, but failed to truncate disk file: {}", escape(&e.to_string())),
            )
            .await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::NewMaintainer;

    fn dev(codename: &str, name: &str, brand: &str, maintainer: Option<(&str, &str)>) -> DeviceSummary {
        DeviceSummary {
            codename: codename.into(),
            name: name.into(),
            brand: brand.into(),
            maintainer: maintainer.map(|(n, g)| NewMaintainer {
                name: n.into(),
                github_url: Some(g.into()),
            }),
            version: None,
            datetime: None,
            changelog: None,
            released: None,
        }
    }

    fn unified() -> UnifiedDevice {
        UnifiedDevice {
            device_name: "POCO <F6>".into(),
            codename: "peridot".into(),
            maintainer: "A&B".into(),
            maintainer_github: Some("https://github.com/a\"b".into()),
            version: "v4.0".into(),
            android_version: "17".into(),
            status: "Active".into(),
            date: "x".into(),
            size: "1 GB".into(),
            gms_download_url: Some("https://ota.example/gms.zip".into()),
            vanilla_download_url: None,
            security_patch: "October 2026 (est.)".into(),
            md5: "abc".into(),
        }
    }

    // --- changelog ---

    #[test]
    fn changelog_formats_structure() {
        let raw = "===== v4.0 =====\nDevice: peridot\n* Fixed <things> & stuff\n\n===== v3.9 =====\n* Old\n===== v3.8 =====\n* Ignored";
        let (out, truncated) = format_changelog_text(raw, 3000);
        assert!(!truncated);
        assert!(out.contains("📌 <b>v4.0</b>"));
        assert!(out.contains("<i>Device: peridot</i>"));
        assert!(out.contains("• Fixed &lt;things&gt; &amp; stuff"));
        assert!(out.contains("v3.9") && !out.contains("v3.8"), "only the latest two versions");
    }

    #[test]
    fn changelog_truncation_never_splits_tags() {
        let raw = format!("===== v1 =====\n{}", "* item with <b>html</b> & more text\n".repeat(500));
        for limit in [50, 123, 500, 3800] {
            let (out, truncated) = format_changelog_text(&raw, limit);
            assert!(truncated);
            assert!(out.len() <= limit);
            assert_eq!(out.matches("<b>").count(), out.matches("</b>").count(), "limit {limit}");
            assert!(!out.contains("<b>html"), "user text must be escaped");
        }
    }

    #[test]
    fn unstructured_changelog_falls_back_to_plain_text() {
        let (out, truncated) = format_changelog_text("just <text>\nmore", 1000);
        assert!(!truncated);
        assert_eq!(out, "just &lt;text&gt;\nmore\n");
    }

    // --- guide ---

    #[test]
    fn guide_keeps_balanced_code_blocks_only() {
        assert_eq!(
            sanitize_guide_html("run <code>fastboot flash</code> & wait"),
            "run <code>fastboot flash</code> &amp; wait"
        );
        assert_eq!(
            sanitize_guide_html("<pre><code>a < b</code></pre>"),
            "<pre><code>a &lt; b</code></pre>"
        );
        // Unbalanced: everything is escaped so Telegram cannot reject the message.
        assert_eq!(sanitize_guide_html("oops <code>never closed"), "oops &lt;code&gt;never closed");
    }

    #[test]
    fn guide_is_truncated_on_step_boundaries() {
        let steps: Vec<InstallationStep> = (0..200)
            .map(|i| InstallationStep {
                title: format!("Step {i}"),
                description: "do <code>this</code> carefully ".repeat(5),
            })
            .collect();
        let text = build_guide_text("Phone", "code", &steps);
        assert!(text.len() < 4096);
        assert!(text.contains("(Guide truncated"));
        assert_eq!(text.matches("<code>").count(), text.matches("</code>").count());
    }

    // --- maintainers ---

    #[test]
    fn maintainer_profile_only_lists_that_maintainers_devices() {
        let devices = vec![
            dev("fog", "Moto G", "Motorola", Some(("Alice", "https://github.com/alice"))),
            dev("pong", "Moto Edge", "Motorola", Some(("Bob", "https://github.com/bob"))),
            dev("nabu", "Pad 5", "Xiaomi", Some(("Alice", "https://github.com/alice"))),
        ];
        // "github" matches both people's profile URLs; the old code then listed everyone's
        // devices under the first match's name.
        let text = maintainer_text(&devices, "github");
        assert!(text.contains("<b>Name:</b> Alice"));
        assert!(text.contains("Moto G") && text.contains("Pad 5"));
        assert!(!text.contains("Moto Edge"), "other maintainers' devices must not be listed");
    }

    #[test]
    fn maintainer_overview_and_no_match() {
        let devices = vec![dev("fog", "Moto G", "Motorola", None)];
        assert!(maintainer_text(&devices, "").contains("Unknown"));
        assert!(maintainer_text(&devices, "zzz").contains("No maintainer found"));
    }

    // --- catalog ---

    #[test]
    fn brands_are_normalized_and_ordered() {
        assert_eq!(normalize_brand("Redmi"), "Xiaomi / Redmi / Poco");
        assert_eq!(normalize_brand("Mi"), "Xiaomi / Redmi / Poco");
        assert_eq!(normalize_brand("Lumi Labs"), "Lumi Labs"); // no false positive on "mi"
        let devices = vec![
            dev("a", "A", "Nothing", None),
            dev("b", "B", "Motorola", None),
            dev("c", "C", "POCO", None),
            dev("d", "D", "Acme", None),
        ];
        assert_eq!(
            get_sorted_brands(&devices),
            vec!["Xiaomi / Redmi / Poco", "Motorola", "Nothing", "Acme"]
        );
    }

    #[test]
    fn huge_catalog_stays_under_telegram_limit() {
        let devices: Vec<DeviceSummary> = (0..500)
            .map(|i| dev(&format!("code{i}"), &format!("Device number {i}"), "Motorola", None))
            .collect();
        let text = full_catalog_text(&devices);
        assert!(text.len() < 4096, "len = {}", text.len());
        assert!(text.contains("more. Browse by brand"));
        assert!(callback_view(&devices, "brand:Motorola").unwrap().0.len() < 4096);
    }

    #[test]
    fn callbacks_route_correctly() {
        let devices = vec![dev("a", "A", "Motorola", None)];
        assert!(callback_view(&devices, "list:menu").unwrap().0.contains("1 Total"));
        assert!(callback_view(&devices, "list:all").is_some());
        assert!(callback_view(&devices, "brand:Motorola").unwrap().0.contains("Motorola Devices (1 supported)"));
        assert!(callback_view(&devices, "garbage").is_none());
    }

    // --- device card ---

    #[test]
    fn device_card_escapes_remote_data() {
        let caption = device_caption(&unified());
        assert!(caption.contains("POCO &lt;F6&gt;"));
        assert!(caption.contains("A&amp;B"));
        assert!(caption.contains("href=\"https://github.com/a%22b\""), "{caption}");
        assert!(caption.contains("🟢 Active"));
    }

    #[test]
    fn download_buttons_only_for_a17() {
        let mut d = unified();
        assert_eq!(device_keyboard(&d).inline_keyboard.len(), 2); // downloads + website
        d.android_version = "16".into();
        assert_eq!(device_keyboard(&d).inline_keyboard.len(), 1); // website only
    }
}
