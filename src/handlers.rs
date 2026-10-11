use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::sync::Arc;
use teloxide::{
    prelude::*,
    types::{CallbackQuery, InlineKeyboardButton, InlineKeyboardMarkup, InputFile, ParseMode},
};
use reqwest::Url;
use sysinfo::{System, Disks};
use std::time::Instant;

use crate::{config, helpers, state::AppState, Command};

// Image asset embedded into binary
const BANNER_BYTES: &[u8] = include_bytes!("../assets/banner.png");

pub async fn command_handler(
    bot: Bot,
    msg: Message,
    cmd: Command,
    state: Arc<AppState>,
) -> Result<(), teloxide::RequestError> {
    state.listener_error_handler.notify_success();

    let user_id = msg.from().map_or(UserId(0), |u| u.id);
    let username = msg.from()
        .and_then(|u| u.username.as_ref())
        .map(|s| s.as_str())
        .unwrap_or("Unknown");

    log::info!("Command `{:?}` from user `{}` ({})", cmd, username, user_id);

    if !config::is_admin(user_id) && !config::is_allowed(msg.chat.id) {
        bot.send_message(
            msg.chat.id,
            format!(
                "❌ <b>This bot only works in The Clover Project group!</b>\n\n📢 <b>Join our group to use this bot:</b>\n{}",
                config::GROUP_LINK
            )
        ).parse_mode(ParseMode::Html).await?;
        return Ok(());
    }

    helpers::stats::track_user(user_id, &state).await;

    match cmd {
        Command::Start => start(bot, msg).await?,
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
        Command::Reboot => reboot(bot, msg).await?,
        Command::Log(args) => log_cmd(bot, msg, args, state).await?,
        Command::Clearlogs => clearlogs(bot, msg, state).await?,
    };

    Ok(())
}

async fn start(bot: Bot, msg: Message) -> Result<(), teloxide::RequestError> {
    let sender_id = msg.from().map_or(UserId(0), |u| u.id);

    let mut welcome = "🤖 <b>Welcome to the Official Clover ROM Bot!</b>\n\n".to_string();
    welcome.push_str("📱 <b>Device Commands:</b>\n");
    welcome.push_str("• /clover &lt;codename&gt; - Get device info & download links\n");
    welcome.push_str("• /changelog &lt;codename&gt; - View device build changes\n");
    welcome.push_str("• /flash &lt;codename&gt; - Step-by-step flashing guide\n");
    welcome.push_str("• /list - Interactive device catalog by manufacturer\n");
    welcome.push_str("• /maintainer [name] - View maintainers & their devices\n\n");
    welcome.push_str("👥 <b>Maintainership:</b>\n");
    welcome.push_str("• /maintainership - Apply for maintainership\n");

    let is_special_user =
        msg.chat.id == config::SPECIAL_GROUP || config::is_admin(sender_id);

    if is_special_user {
        welcome.push_str("\n⚙️ <b>Server Commands:</b>\n");
        welcome.push_str("• /uptime - Bot uptime\n");
        welcome.push_str("• /ping - Bot response time\n");
        welcome.push_str("• /stats - Bot usage stats\n");
        welcome.push_str("• /server - Server details\n");
    }

    if config::is_admin(sender_id) {
        welcome.push_str("\n🔧 <b>Admin Commands:</b>\n");
        welcome.push_str("• /sync - Resync device catalog from GitHub\n");
        welcome.push_str("• /reboot - Restart the bot\n");
        welcome.push_str("• /log [lines|file] - View or download bot logs\n");
        welcome.push_str("• /clearlogs - Clear bot logs\n");
        welcome.push_str("• /clover insert &lt;device&gt; &lt;status&gt; - Set status\n");
        welcome.push_str("• /clover remove &lt;device&gt; - Remove status\n");
    }

    bot.send_message(msg.chat.id, welcome)
        .reply_to_message_id(msg.id)
        .parse_mode(ParseMode::Html)
        .await?;
    Ok(())
}

async fn clover_info(
    bot: Bot,
    msg: Message,
    raw_args: String,
    state: Arc<AppState>,
) -> Result<(), teloxide::RequestError> {
    let trimmed_args = raw_args.trim();
    let args: Vec<&str> = trimmed_args.split_whitespace().collect();
    let command = args.first().copied().unwrap_or("").to_lowercase();
    
    let user_id = msg.from().map_or(UserId(0), |u| u.id);

    // --- 1. HANDLE "INSERT" COMMAND ---
    if command == "insert" {
        if !config::is_admin(user_id) {
            bot.send_message(msg.chat.id, "❌ Admin only.").reply_to_message_id(msg.id).await?;
            return Ok(());
        }
        if args.len() < 3 {
            bot.send_message(msg.chat.id, "❌ Usage: <code>/clover insert &lt;device&gt; &lt;status&gt;</code>")
                .parse_mode(ParseMode::Html).reply_to_message_id(msg.id).await?;
            return Ok(());
        }
        let codename = args[1].to_lowercase();
        let status = args[2..].join(" ").replace('"', "");

        let mut statuses = state.device_statuses.write().await;
        statuses.insert(codename.clone(), status.clone());
        crate::helpers::storage::save_statuses(&statuses);

        bot.send_message(msg.chat.id, format!("✅ Set status for <b>{}</b> to <b>{}</b>", codename, teloxide::utils::html::escape(&status)))
            .parse_mode(ParseMode::Html).reply_to_message_id(msg.id).await?;
        return Ok(());
    }

    // --- 2. HANDLE "REMOVE" COMMAND ---
    if command == "remove" {
        if !config::is_admin(user_id) {
            bot.send_message(msg.chat.id, "❌ Admin only.").reply_to_message_id(msg.id).await?;
            return Ok(());
        }
        if args.len() < 2 {
            bot.send_message(msg.chat.id, "❌ Usage: <code>/clover remove &lt;device&gt;</code>")
                .parse_mode(ParseMode::Html).reply_to_message_id(msg.id).await?;
            return Ok(());
        }
        let codename = args[1].to_lowercase();

        let mut statuses = state.device_statuses.write().await;
        if statuses.remove(&codename).is_some() {
            crate::helpers::storage::save_statuses(&statuses);
            bot.send_message(msg.chat.id, format!("✅ Removed custom status for <b>{}</b>", codename))
                .parse_mode(ParseMode::Html).reply_to_message_id(msg.id).await?;
        } else {
            bot.send_message(msg.chat.id, format!("⚠️ No custom status found for <b>{}</b>", codename))
                .parse_mode(ParseMode::Html).reply_to_message_id(msg.id).await?;
        }
        return Ok(());
    }

    // --- 3. HANDLE "INACTIVE" COMMAND ---
    if command == "inactive" {
        let statuses = state.device_statuses.read().await;
        if statuses.is_empty() {
            bot.send_message(msg.chat.id, "ℹ️ No devices are manually marked as Inactive.").reply_to_message_id(msg.id).await?;
            return Ok(());
        }

        let mut list_text = "🚫 <b>Inactive / Custom Status Devices:</b>\n\n".to_string();
        for (code, stat) in statuses.iter() {
            list_text.push_str(&format!("• <b>{}</b>: {}\n", teloxide::utils::html::escape(code), teloxide::utils::html::escape(stat)));
        }

        bot.send_message(msg.chat.id, list_text).parse_mode(ParseMode::Html).reply_to_message_id(msg.id).await?;
        return Ok(());
    }

    // --- 4. STANDARD FETCH LOGIC ---
    let codename = trimmed_args;

    if codename.is_empty() {
        bot.send_message(msg.chat.id, "❌ Please specify a device codename.\nExample: <code>/clover peridot</code>")
            .reply_to_message_id(msg.id).parse_mode(ParseMode::Html).await?;
        return Ok(());
    }

    // A. Send "Fetching..." placeholder
    let status_msg = bot.send_message(msg.chat.id, format!("🔍 <b>Fetching '{}'...</b>", teloxide::utils::html::escape(codename)))
        .reply_to_message_id(msg.id)
        .parse_mode(ParseMode::Html)
        .await?;

    // B. Perform GitHub fetch
    let fetch_result = helpers::github::load_device_from_github(codename, &state).await;

    match fetch_result {
        Ok(Some(device)) => {
            {
                let mut stats = state.stats.write().await;
                stats.devices_queried += 1;
            }

            let status_emoji = if device.status == "Active" || device.status == "Stable" { "🟢" } else { "🔴" };
            let status_with_emoji = format!("{} {}", status_emoji, teloxide::utils::html::escape(&device.status));

            let maintainer_display = match &device.maintainer_github {
                Some(url) if !url.is_empty() => format!("<a href=\"{}\">{}</a>", url, teloxide::utils::html::escape(&device.maintainer)),
                _ => teloxide::utils::html::escape(&device.maintainer),
            };

            let caption_text = format!(
                "📱 <b>Device</b>\n\
                <b>{}</b> — <code>{}</code>\n\n\
                🛠 <b>Build Info</b>\n\
                <b>Status:</b> {}\n\
                <b>Version:</b> {} (Android {})\n\
                <b>Date:</b> {}\n\
                <b>File Size:</b> {}\n\n\
                🛡 <b>Security</b>\n\
                <b>Patch Level:</b> {}\n\
                <b>MD5:</b> <code>{}</code>\n\n\
                🧑‍💻 <b>Maintainer:</b> {}\n\n\
                💡 <i>Commands: <code>/flash {}</code> • <code>/changelog {}</code></i>",
                teloxide::utils::html::escape(&device.device_name),
                teloxide::utils::html::escape(&device.codename),
                status_with_emoji,
                teloxide::utils::html::escape(&device.version),
                teloxide::utils::html::escape(&device.android_version),
                teloxide::utils::html::escape(&device.date),
                teloxide::utils::html::escape(&device.size),
                teloxide::utils::html::escape(&device.security_patch),
                teloxide::utils::html::escape(&device.md5),
                maintainer_display,
                teloxide::utils::html::escape(&device.codename),
                teloxide::utils::html::escape(&device.codename)
            );

            // Separate GMS & Vanilla Download Buttons
            let mut keyboard_rows: Vec<Vec<InlineKeyboardButton>> = Vec::new();
            let mut download_row: Vec<InlineKeyboardButton> = Vec::new();

            if let Some(ref gms) = device.gms_download_url {
                if let Ok(url) = Url::parse(gms) {
                    download_row.push(InlineKeyboardButton::url("📦 Download GMS", url));
                }
            }

            if let Some(ref vanilla) = device.vanilla_download_url {
                if let Ok(url) = Url::parse(vanilla) {
                    download_row.push(InlineKeyboardButton::url("🍦 Download Vanilla", url));
                }
            }

            if download_row.is_empty() {
                let download_url = Url::parse(&device.download_url)
                    .unwrap_or_else(|_| Url::parse("https://thecloverproject.com").unwrap());
                download_row.push(InlineKeyboardButton::url("📥 Download Now", download_url));
            }
            keyboard_rows.push(download_row);

            let website_url_str = format!("https://thecloverproject.com/download/{}", device.codename);
            let website_url = Url::parse(&website_url_str)
                .unwrap_or_else(|_| Url::parse("https://thecloverproject.com").unwrap());

            keyboard_rows.push(vec![
                InlineKeyboardButton::url("🌐 Website", website_url),
            ]);

            let keyboard = InlineKeyboardMarkup::new(keyboard_rows);

            // C. Efficient Banner Image Delivery using FileId Caching
            let cached_file_id = {
                let guard = state.banner_file_id.read().await;
                guard.clone()
            };

            let input_banner = match cached_file_id {
                Some(ref file_id) => InputFile::file_id(file_id.clone()),
                None => InputFile::memory(BANNER_BYTES).file_name("banner.png"),
            };

            let sent_msg = bot.send_photo(msg.chat.id, input_banner)
                .caption(caption_text)
                .reply_to_message_id(msg.id)
                .parse_mode(ParseMode::Html)
                .reply_markup(keyboard)
                .await?;

            // If this was the first upload, cache the Telegram FileId
            if cached_file_id.is_none() {
                if let Some(photos) = sent_msg.photo() {
                    if let Some(largest) = photos.last() {
                        let mut guard = state.banner_file_id.write().await;
                        *guard = Some(largest.file.id.clone());
                        log::info!("Cached Telegram file_id for banner image: {}", largest.file.id);
                    }
                }
            }

            // D. Delete "Fetching..." placeholder after success
            let _ = bot.delete_message(msg.chat.id, status_msg.id).await;
        }
        Ok(None) => {
            let _ = bot.delete_message(msg.chat.id, status_msg.id).await;
            bot.send_message(msg.chat.id, format!(
                    "❌ No information for <code>{}</code>\nUse /list to see available devices.",
                    teloxide::utils::html::escape(codename)
                ),
            )
            .reply_to_message_id(msg.id)
            .parse_mode(ParseMode::Html)
            .await?;
        }
        Err(e) => {
            let _ = bot.delete_message(msg.chat.id, status_msg.id).await;
            log::error!("Error fetching device info for {}: {}", codename, e);
            bot.send_message(msg.chat.id, "❌ An error occurred while fetching device data.")
                .reply_to_message_id(msg.id)
                .await?;
        }
    }
    Ok(())
}

fn format_changelog_text(raw: &str) -> String {
    let mut out = String::new();
    let mut version_count = 0;

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if !out.ends_with("\n\n") {
                out.push('\n');
            }
            continue;
        }

        if trimmed.starts_with("=====") {
            let title = trimmed.trim_matches('=').trim();
            if version_count >= 2 {
                break;
            }
            version_count += 1;
            out.push_str(&format!("📌 <b>{}</b>\n", teloxide::utils::html::escape(title)));
        } else if trimmed.starts_with('*') || trimmed.starts_with('•') {
            let item = trimmed.trim_start_matches('*').trim_start_matches('•').trim();
            out.push_str(&format!("• {}\n", teloxide::utils::html::escape(item)));
        } else if trimmed.starts_with("Highlights &")
            || trimmed.starts_with("Build type:")
            || trimmed.starts_with("Device:")
            || trimmed.starts_with("Device maintainer:")
            || trimmed.starts_with("Credits:")
        {
            out.push_str(&format!("<i>{}</i>\n", teloxide::utils::html::escape(trimmed)));
        } else {
            out.push_str(&format!("{}\n", teloxide::utils::html::escape(trimmed)));
        }
    }

    if out.trim().is_empty() {
        teloxide::utils::html::escape(raw)
    } else {
        out
    }
}

async fn changelog_cmd(
    bot: Bot,
    msg: Message,
    raw_args: String,
    state: Arc<AppState>,
) -> Result<(), teloxide::RequestError> {
    let codename = raw_args.trim();
    if codename.is_empty() {
        bot.send_message(
            msg.chat.id,
            "❌ Please specify a device codename.\nExample: <code>/changelog peridot</code>",
        )
        .reply_to_message_id(msg.id)
        .parse_mode(ParseMode::Html)
        .await?;
        return Ok(());
    }

    let status_msg = bot.send_message(
        msg.chat.id,
        format!("🔍 <b>Fetching changelog for '{}'...</b>", teloxide::utils::html::escape(codename)),
    )
    .reply_to_message_id(msg.id)
    .parse_mode(ParseMode::Html)
    .await?;

    let changelog_res = helpers::github::fetch_device_changelog(codename, &state).await;
    let _ = bot.delete_message(msg.chat.id, status_msg.id).await;

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

            let formatted_body = format_changelog_text(&raw_text);
            let mut text = format!(
                "📜 <b>Changelog — {}</b> (<code>{}</code>)\n━━━━━━━━━━━━━━━━━━━━━━\n\n{}",
                teloxide::utils::html::escape(&display_name),
                teloxide::utils::html::escape(codename),
                formatted_body
            );

            let web_url = Url::parse(&format!("https://thecloverproject.com/download/{}", codename))
                .unwrap_or_else(|_| Url::parse("https://thecloverproject.com").unwrap());
            let keyboard = InlineKeyboardMarkup::new(vec![
                vec![InlineKeyboardButton::url("🌐 View on Website", web_url)],
            ]);

            if text.len() > 3800 {
                let cutoff = text.floor_char_boundary(3600);
                text = format!("{}...\n\n<i>(Changelog truncated. View full changelog on website.)</i>", &text[..cutoff]);
            }

            bot.send_message(msg.chat.id, text)
                .reply_to_message_id(msg.id)
                .parse_mode(ParseMode::Html)
                .reply_markup(keyboard)
                .await?;
        }
        Ok(None) => {
            bot.send_message(
                msg.chat.id,
                format!("ℹ️ No specific changelog found for <code>{}</code>. Use /list to see supported devices.", teloxide::utils::html::escape(codename)),
            )
            .reply_to_message_id(msg.id)
            .parse_mode(ParseMode::Html)
            .await?;
        }
        Err(e) => {
            log::error!("Error fetching changelog for {}: {}", codename, e);
            bot.send_message(msg.chat.id, "❌ An error occurred while fetching device changelog.")
                .reply_to_message_id(msg.id)
                .await?;
        }
    }

    Ok(())
}

fn sanitize_guide_html(text: &str) -> String {
    let p1 = text
        .replace("<pre><code>", "___PRE_CODE___")
        .replace("</code></pre>", "___END_PRE_CODE___")
        .replace("<code>", "___CODE___")
        .replace("</code>", "___END_CODE___");
    let escaped = teloxide::utils::html::escape(&p1);
    escaped
        .replace("___PRE_CODE___", "<pre><code>")
        .replace("___END_PRE_CODE___", "</code></pre>")
        .replace("___CODE___", "<code>")
        .replace("___END_CODE___", "</code>")
}

async fn flash_cmd(
    bot: Bot,
    msg: Message,
    raw_args: String,
    state: Arc<AppState>,
) -> Result<(), teloxide::RequestError> {
    let codename = raw_args.trim();
    if codename.is_empty() {
        bot.send_message(
            msg.chat.id,
            "❌ Please specify a device codename.\nExample: <code>/flash peridot</code>",
        )
        .reply_to_message_id(msg.id)
        .parse_mode(ParseMode::Html)
        .await?;
        return Ok(());
    }

    let status_msg = bot.send_message(
        msg.chat.id,
        format!("🔍 <b>Fetching flashing guide for '{}'...</b>", teloxide::utils::html::escape(codename)),
    )
    .reply_to_message_id(msg.id)
    .parse_mode(ParseMode::Html)
    .await?;

    let site_info = helpers::github::fetch_device_site_info(codename, &state).await;
    let _ = bot.delete_message(msg.chat.id, status_msg.id).await;

    match site_info {
        Ok(Some(root)) => {
            let guide = root.installation_guide.map(|g| g.steps).unwrap_or_default();
            if guide.is_empty() {
                bot.send_message(
                    msg.chat.id,
                    format!("ℹ️ No specific flashing guide found for <b>{}</b> (<code>{}</code>).",
                        teloxide::utils::html::escape(&root.name),
                        teloxide::utils::html::escape(codename)
                    ),
                )
                .reply_to_message_id(msg.id)
                .parse_mode(ParseMode::Html)
                .await?;
                return Ok(());
            }

            let mut text = format!(
                "📖 <b>Flashing Guide — {}</b> (<code>{}</code>)\n━━━━━━━━━━━━━━━━━━━━━━\n\n",
                teloxide::utils::html::escape(&root.name),
                teloxide::utils::html::escape(codename)
            );

            for (idx, step) in guide.iter().enumerate() {
                text.push_str(&format!(
                    "<b>{}. {}</b>\n{}\n\n",
                    idx + 1,
                    teloxide::utils::html::escape(&step.title),
                    sanitize_guide_html(&step.description)
                ));
            }

            let web_url = Url::parse(&format!("https://thecloverproject.com/download/{}", codename))
                .unwrap_or_else(|_| Url::parse("https://thecloverproject.com").unwrap());
            let keyboard = InlineKeyboardMarkup::new(vec![
                vec![InlineKeyboardButton::url("🌐 Web Guide & Downloads", web_url)],
            ]);

            if text.len() > 3800 {
                let cutoff = text.floor_char_boundary(3600);
                text = format!("{}...\n\n<i>(Guide truncated. View full guide on website.)</i>", &text[..cutoff]);
            }

            bot.send_message(msg.chat.id, text)
                .reply_to_message_id(msg.id)
                .parse_mode(ParseMode::Html)
                .reply_markup(keyboard)
                .await?;
        }
        Ok(None) => {
            bot.send_message(
                msg.chat.id,
                format!("❌ Device <code>{}</code> not found. Use /list to see supported devices.", teloxide::utils::html::escape(codename)),
            )
            .reply_to_message_id(msg.id)
            .parse_mode(ParseMode::Html)
            .await?;
        }
        Err(e) => {
            log::error!("Error fetching flashing guide for {}: {}", codename, e);
            bot.send_message(msg.chat.id, "❌ An error occurred while fetching flashing guide.")
                .reply_to_message_id(msg.id)
                .await?;
        }
    }

    Ok(())
}

async fn maintainer_cmd(
    bot: Bot,
    msg: Message,
    raw_args: String,
    state: Arc<AppState>,
) -> Result<(), teloxide::RequestError> {
    let query = raw_args.trim().to_lowercase();
    let devices = state.devices_cache.read().await;

    if devices.is_empty() {
        bot.send_message(msg.chat.id, "❌ Device list is loading... Please try again in a moment.")
            .reply_to_message_id(msg.id)
            .await?;
        return Ok(());
    }

    if query.is_empty() {
        let mut by_maintainer: std::collections::BTreeMap<String, Vec<String>> = std::collections::BTreeMap::new();

        for dev in devices.iter() {
            let m_name = dev.maintainer.as_ref().map(|m| m.name.clone()).unwrap_or_else(|| "Unknown".to_string());
            by_maintainer.entry(m_name).or_default().push(dev.codename.clone());
        }

        let mut text = "👥 <b>The Clover Project Maintainers</b>\n━━━━━━━━━━━━━━━━━━━━━━\n\n".to_string();
        for (maintainer, codenames) in by_maintainer {
            text.push_str(&format!("• <b>{}</b> — {}\n", teloxide::utils::html::escape(&maintainer), codenames.join(", ")));
        }
        text.push_str("\n<i>💡 Tip: Use <code>/maintainer &lt;name&gt;</code> for details.</i>");

        bot.send_message(msg.chat.id, text)
            .reply_to_message_id(msg.id)
            .parse_mode(ParseMode::Html)
            .await?;
        return Ok(());
    }

    let mut matches: Vec<&crate::models::DeviceSummary> = Vec::new();
    for dev in devices.iter() {
        if let Some(ref m) = dev.maintainer {
            if m.name.to_lowercase().contains(&query)
                || m.github_url.as_ref().is_some_and(|u| u.to_lowercase().contains(&query))
                || dev.codename.to_lowercase() == query {
                matches.push(dev);
            }
        }
    }

    if matches.is_empty() {
        bot.send_message(
            msg.chat.id,
            format!("❌ No maintainer found matching '<b>{}</b>'. Use <code>/maintainer</code> to see all maintainers.", teloxide::utils::html::escape(&query)),
        )
        .reply_to_message_id(msg.id)
        .parse_mode(ParseMode::Html)
        .await?;
        return Ok(());
    }

    let first = matches[0];
    let m = first.maintainer.as_ref().unwrap();
    let github_link = match &m.github_url {
        Some(url) if !url.is_empty() => format!("<a href=\"{}\">{}</a>", url, url),
        _ => "N/A".to_string(),
    };

    let mut text = format!(
        "🧑‍💻 <b>Maintainer Profile</b>\n━━━━━━━━━━━━━━━━━━━━━━\n\
        👤 <b>Name:</b> {}\n\
        🔗 <b>GitHub:</b> {}\n\n\
        📱 <b>Maintained Devices:</b>\n",
        teloxide::utils::html::escape(&m.name),
        github_link
    );

    for dev in matches {
        text.push_str(&format!("• {} (<code>{}</code>)\n", teloxide::utils::html::escape(&dev.name), teloxide::utils::html::escape(&dev.codename)));
    }

    bot.send_message(msg.chat.id, text)
        .reply_to_message_id(msg.id)
        .parse_mode(ParseMode::Html)
        .await?;

    Ok(())
}

// --- INTERACTIVE /LIST & BRAND HELPERS ---

pub fn normalize_brand(raw_brand: &str) -> String {
    let lower = raw_brand.to_lowercase();
    if lower.contains("xiaomi") 
        || lower.contains("redmi") 
        || lower.contains("poco") 
        || lower.contains("mi ") 
        || lower == "mi" {
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

pub fn get_sorted_brands(devices: &[crate::models::DeviceSummary]) -> Vec<String> {
    let mut brands_set: std::collections::HashSet<String> = std::collections::HashSet::new();
    for dev in devices {
        brands_set.insert(normalize_brand(&dev.brand));
    }

    let mut brands: Vec<String> = brands_set.into_iter().collect();

    let get_priority = |b: &str| -> i32 {
        if b.contains("Xiaomi") { 0 }
        else if b.contains("Motorola") { 1 }
        else if b.contains("Realme") { 2 }
        else if b.contains("Nothing") { 3 }
        else { 4 }
    };

    brands.sort_by(|a, b| {
        let prio_a = get_priority(a);
        let prio_b = get_priority(b);
        if prio_a != prio_b {
            prio_a.cmp(&prio_b)
        } else {
            a.cmp(b)
        }
    });

    brands
}

pub fn make_brand_menu_keyboard(brands: &[String]) -> InlineKeyboardMarkup {
    let mut rows: Vec<Vec<InlineKeyboardButton>> = Vec::new();
    let mut current_row: Vec<InlineKeyboardButton> = Vec::new();

    for brand in brands {
        let cb_data = format!("brand:{}", brand);
        current_row.push(InlineKeyboardButton::callback(
            format!("📱 {}", brand),
            cb_data,
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

pub fn build_full_catalog_text(devices: &[crate::models::DeviceSummary]) -> String {
    let mut grouped: std::collections::HashMap<String, Vec<&crate::models::DeviceSummary>> = std::collections::HashMap::new();

    for device in devices.iter() {
        let display_brand = normalize_brand(&device.brand);
        grouped.entry(display_brand).or_default().push(device);
    }

    let brands = get_sorted_brands(devices);
    let mut msg_text = "📋 <b>List of Supported Devices:</b>\n\n".to_string();

    for brand in brands {
        if let Some(dev_list) = grouped.get(&brand) {
            msg_text.push_str(&format!("<b>{}</b>\n", teloxide::utils::html::escape(&brand)));
            
            let mut sorted_devs = dev_list.clone();
            sorted_devs.sort_by(|a, b| a.name.cmp(&b.name));

            for dev in sorted_devs {
                msg_text.push_str(&format!(
                    "• {} (<code>{}</code>)\n",
                    teloxide::utils::html::escape(&dev.name),
                    teloxide::utils::html::escape(&dev.codename)
                ));
            }
            msg_text.push('\n');
        }
    }

    msg_text.push_str("Use /clover &lt;codename&gt; to get details.");
    msg_text
}

async fn list_devices(
    bot: Bot,
    msg: Message,
    state: Arc<AppState>,
) -> Result<(), teloxide::RequestError> {
    let devices = state.devices_cache.read().await;
    
    if devices.is_empty() {
        bot.send_message(msg.chat.id, "❌ Device list is empty or loading... Try again in a moment.")
            .reply_to_message_id(msg.id)
            .await?;
        return Ok(());
    }

    let brands = get_sorted_brands(&devices);
    let keyboard = make_brand_menu_keyboard(&brands);
    let text = format!(
        "📋 <b>The Clover Project — Supported Devices ({} Total)</b>\n\n\
        Select a manufacturer below to browse devices, or view the complete catalog:",
        devices.len()
    );

    bot.send_message(msg.chat.id, text)
        .reply_to_message_id(msg.id)
        .parse_mode(ParseMode::Html)
        .reply_markup(keyboard)
        .await?;
        
    Ok(())
}

// --- CALLBACK QUERY HANDLER (Brand navigation) ---
pub async fn callback_handler(
    bot: Bot,
    q: CallbackQuery,
    state: Arc<AppState>,
) -> Result<(), teloxide::RequestError> {
    state.listener_error_handler.notify_success();

    let Some(ref data) = q.data else {
        bot.answer_callback_query(q.id).await?;
        return Ok(());
    };

    let Some(ref msg) = q.message else {
        bot.answer_callback_query(q.id).await?;
        return Ok(());
    };

    bot.answer_callback_query(q.id).await?;

    if data == "list:menu" {
        let devices = state.devices_cache.read().await;
        let brands = get_sorted_brands(&devices);
        let keyboard = make_brand_menu_keyboard(&brands);
        let text = format!(
            "📋 <b>The Clover Project — Supported Devices ({} Total)</b>\n\n\
            Select a manufacturer below to browse devices, or view the complete catalog:",
            devices.len()
        );

        let _ = bot.edit_message_text(msg.chat.id, msg.id, text)
            .parse_mode(ParseMode::Html)
            .reply_markup(keyboard)
            .await;
    } else if data == "list:all" {
        let devices = state.devices_cache.read().await;
        let text = build_full_catalog_text(&devices);
        let keyboard = InlineKeyboardMarkup::new(vec![
            vec![InlineKeyboardButton::callback("⬅️ Back to Brands", "list:menu")],
        ]);

        let _ = bot.edit_message_text(msg.chat.id, msg.id, text)
            .parse_mode(ParseMode::Html)
            .reply_markup(keyboard)
            .await;
    } else if let Some(target_brand) = data.strip_prefix("brand:") {
        let devices = state.devices_cache.read().await;
        let mut matching_devices: Vec<&crate::models::DeviceSummary> = devices
            .iter()
            .filter(|d| normalize_brand(&d.brand) == target_brand)
            .collect();

        matching_devices.sort_by(|a, b| a.name.cmp(&b.name));

        let mut text = format!(
            "📱 <b>{} Devices ({} supported):</b>\n━━━━━━━━━━━━━━━━━━━━━━\n\n",
            teloxide::utils::html::escape(target_brand),
            matching_devices.len()
        );

        for dev in matching_devices {
            text.push_str(&format!(
                "• {} (<code>{}</code>)\n",
                teloxide::utils::html::escape(&dev.name),
                teloxide::utils::html::escape(&dev.codename)
            ));
        }

        text.push_str("\n💡 <i>Use <code>/clover &lt;codename&gt;</code> for downloads & details.</i>");

        let keyboard = InlineKeyboardMarkup::new(vec![
            vec![
                InlineKeyboardButton::callback("⬅️ Back to Brands", "list:menu"),
                InlineKeyboardButton::callback("🌐 Full Catalog", "list:all"),
            ],
        ]);

        let _ = bot.edit_message_text(msg.chat.id, msg.id, text)
            .parse_mode(ParseMode::Html)
            .reply_markup(keyboard)
            .await;
    }

    Ok(())
}

async fn maintainership(bot: Bot, msg: Message) -> Result<(), teloxide::RequestError> {
    let text = "👥 <b>Maintainership Application</b>\n\n\
        Interested in maintaining Clover ROM for your device?\n\n\
        📋 <b>Requirements:</b>\n\
        • Basic knowledge of Android ROM development\n\
        • Device tree experience\n\
        • Commitment to regular updates";

    let url = Url::parse("https://github.com/The-Clover-Project/official_updates/issues/new?template=maintainer_application.yml").unwrap();
    let keyboard = InlineKeyboardMarkup::new(vec![
        vec![InlineKeyboardButton::url("📝 Apply Here", url)],
    ]);

    bot.send_message(msg.chat.id, text)
        .reply_to_message_id(msg.id)
        .parse_mode(ParseMode::Html)
        .reply_markup(keyboard)
        .await?;

    Ok(())
}

async fn uptime(bot: Bot, msg: Message, state: Arc<AppState>) -> Result<(), teloxide::RequestError> {
    let uptime_duration = state.bot_start_time.elapsed();
    let formatted_uptime = helpers::formatting::format_uptime(uptime_duration.as_secs());
    bot.send_message(msg.chat.id, format!("⏰ <b>Bot Uptime:</b> {}", formatted_uptime))
        .reply_to_message_id(msg.id)
        .parse_mode(ParseMode::Html)
        .await?;
    Ok(())
}

async fn ping(bot: Bot, msg: Message) -> Result<(), teloxide::RequestError> {
    let start_time = Instant::now();
    let sent_msg = bot.send_message(msg.chat.id, "🏓 <b>Pinging...</b>")
        .reply_to_message_id(msg.id)
        .parse_mode(ParseMode::Html)
        .await?;
    let rt = start_time.elapsed().as_millis();
    bot.edit_message_text(
        msg.chat.id,
        sent_msg.id,
        format!("🏓 <b>Pong!</b> Response time: {}ms", rt),
    )
    .parse_mode(ParseMode::Html)
    .await?;
    Ok(())
}

async fn stats_command(
    bot: Bot,
    msg: Message,
    state: Arc<AppState>,
) -> Result<(), teloxide::RequestError> {
    let stats = state.stats.read().await;
    let uptime = helpers::formatting::format_uptime(state.bot_start_time.elapsed().as_secs());
    let text = format!(
        "📊 <b>Bot Statistics</b>\n\n\
        • Total unique users: {}\n\
        • Commands processed: {}\n\
        • Devices queried: {}\n\
        • Uptime: {}",
        stats.total_users.len(),
        stats.commands_processed,
        stats.devices_queried,
        uptime
    );
    bot.send_message(msg.chat.id, text).reply_to_message_id(msg.id).parse_mode(ParseMode::Html).await?;
    Ok(())
}

async fn server(bot: Bot, msg: Message) -> Result<(), teloxide::RequestError> {
    let mut sys = System::new_all();
    sys.refresh_cpu();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    sys.refresh_cpu();
    sys.refresh_memory();
     
    let disks = Disks::new_with_refreshed_list();
    let disk_info = if let Some(disk) = disks.first() {
        let total = disk.total_space();
        let used = total.saturating_sub(disk.available_space());
        let pct = if total > 0 { (used as f64 / total as f64) * 100.0 } else { 0.0 };
        format!(
            "{:.2}% ({}GB / {}GB)",
            pct,
            used / 1024 / 1024 / 1024,
            total / 1024 / 1024 / 1024
        )
    } else {
        "N/A".to_string()
    };

    let cpu = sys.global_cpu_info();
    let ram_total = sys.total_memory();
    let ram_used = sys.used_memory();

    let text = format!(
        "🖥 <b>Server Status</b>\n━━━━━━━━━━━━━━━━━━━━━━\n\
        📟 <b>System:</b> {} {}\n\
        💻 <b>CPU Usage:</b> {:.2}%\n\
        🧠 <b>RAM Usage:</b> {:.2}% ({}MB / {}MB)\n\
        💾 <b>Disk Usage:</b> {}\n\
        ⏱ <b>Uptime:</b> {}",
        System::name().unwrap_or_default(), System::os_version().unwrap_or_default(),
        cpu.cpu_usage(),
        (ram_used as f64 / ram_total as f64) * 100.0, ram_used / 1024 / 1024, ram_total / 1024 / 1024,
        disk_info,
        helpers::formatting::format_uptime(System::uptime())
    );

    bot.send_message(msg.chat.id, text).reply_to_message_id(msg.id).parse_mode(ParseMode::Html).await?;
    Ok(())
}

async fn sync_cmd(
    bot: Bot,
    msg: Message,
    state: Arc<AppState>,
) -> Result<(), teloxide::RequestError> {
    let user_id = msg.from().map_or(UserId(0), |u| u.id);
    if !config::is_admin(user_id) {
        bot.send_message(msg.chat.id, "❌ This command is only available for administrators.")
            .reply_to_message_id(msg.id)
            .await?;
        return Ok(());
    }

    let progress_msg = bot.send_message(msg.chat.id, "🔄 <b>Syncing device catalog from GitHub...</b>")
        .reply_to_message_id(msg.id)
        .parse_mode(ParseMode::Html)
        .await?;

    match helpers::github::preload_devices(&state).await {
        Ok(count) => {
            let _ = bot.delete_message(msg.chat.id, progress_msg.id).await;
            bot.send_message(
                msg.chat.id,
                format!("✅ <b>Device catalog synced successfully!</b>\nPreloaded <b>{}</b> devices into memory cache.", count),
            )
            .reply_to_message_id(msg.id)
            .parse_mode(ParseMode::Html)
            .await?;
            log::info!("Admin {} synced device catalog ({} devices)", user_id, count);
        }
        Err(e) => {
            let _ = bot.delete_message(msg.chat.id, progress_msg.id).await;
            bot.send_message(
                msg.chat.id,
                format!("❌ Failed to sync device catalog: {}", teloxide::utils::html::escape(&e.to_string())),
            )
            .reply_to_message_id(msg.id)
            .await?;
            log::error!("Failed to sync device catalog: {}", e);
        }
    }

    Ok(())
}

async fn reboot(bot: Bot, msg: Message) -> Result<(), teloxide::RequestError> {
    let user_id = msg.from().map_or(UserId(0), |u| u.id);
    if !config::is_admin(user_id) {
        bot.send_message(msg.chat.id, "❌ This command is only available for administrators.")
            .reply_to_message_id(msg.id)
            .await?;
        return Ok(());
    }

    bot.send_message(msg.chat.id, "🔄 <b>Rebooting bot...</b>")
        .reply_to_message_id(msg.id)
        .parse_mode(ParseMode::Html)
        .await?;
    log::warn!("Reboot command initiated by admin {}", user_id);

    use std::process::exit;
    exit(1);
}

fn get_logs_from_file(lines_to_read: usize) -> Result<String, std::io::Error> {
    match File::open(config::LOG_FILE) {
        Ok(file) => {
            let reader = BufReader::new(file);
            let lines: Vec<String> = reader.lines().map_while(Result::ok).collect();
            let log_portion = lines.iter().rev().take(lines_to_read).rev().cloned().collect::<Vec<String>>();
            Ok(log_portion.join("\n"))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            Ok(String::new())
        }
        Err(e) => {
            Err(e)
        }
    }
}

async fn log_cmd(bot: Bot, msg: Message, args: String, state: Arc<AppState>) -> Result<(), teloxide::RequestError> {
    let user_id = msg.from().map_or(UserId(0), |u| u.id);
    if !config::is_admin(user_id) {
        bot.send_message(msg.chat.id, "❌ This command is only available for administrators.")
            .reply_to_message_id(msg.id)
            .await?;
        return Ok(());
    }

    let trimmed = args.trim().to_lowercase();

    // If admin requests the log file as a document
    if trimmed == "file" || trimmed == "doc" || trimmed == "download" {
        if tokio::fs::try_exists(config::LOG_FILE).await.unwrap_or(false) {
            let doc = InputFile::file(config::LOG_FILE).file_name("clover_bot.log");
            bot.send_document(msg.chat.id, doc)
                .caption("📄 <b>Full Clover Bot Log File</b>")
                .parse_mode(ParseMode::Html)
                .reply_to_message_id(msg.id)
                .await?;
            return Ok(());
        } else {
            let memory_logs = state.log_buffer.get_all_as_string();
            let doc = InputFile::memory(memory_logs.into_bytes()).file_name("clover_bot.log");
            bot.send_document(msg.chat.id, doc)
                .caption("📄 <b>Clover Bot In-Memory Logs</b>")
                .parse_mode(ParseMode::Html)
                .reply_to_message_id(msg.id)
                .await?;
            return Ok(());
        }
    }

    let lines = trimmed.split_whitespace().next()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(40)
        .clamp(5, 200);

    let log_lines = state.log_buffer.get_last_lines(lines);
    let mut logs = if !log_lines.is_empty() {
        log_lines.join("\n")
    } else {
        match get_logs_from_file(lines) {
            Ok(content) => content,
            Err(e) => {
                log::error!("Failed to read logs from disk: {}", e);
                format!("Error reading logs: {}", e)
            }
        }
    };

    if logs.trim().is_empty() {
        logs = "Log buffer is empty or no events recorded yet.".to_string();
    }

    if logs.len() > 3700 {
        let start_index = logs.len() - 3700;
        let truncated = format!("... (truncated, showing last 3700 characters) ...\n{}", &logs[start_index..]);
        let text = format!(
            "📝 <b>Bot Logs (last {} lines):</b>\n<pre>{}</pre>\n<i>💡 Tip: Use <code>/log file</code> to download the complete log file.</i>",
            lines,
            teloxide::utils::html::escape(&truncated)
        );
        bot.send_message(msg.chat.id, text).reply_to_message_id(msg.id).parse_mode(ParseMode::Html).await?;
    } else {
        let text = format!(
            "📝 <b>Bot Logs (last {} lines):</b>\n<pre>{}</pre>\n<i>💡 Tip: Use <code>/log file</code> to download the full log.</i>",
            lines,
            teloxide::utils::html::escape(&logs)
        );
        bot.send_message(msg.chat.id, text).reply_to_message_id(msg.id).parse_mode(ParseMode::Html).await?;
    }
     
    Ok(())
}

async fn clearlogs(bot: Bot, msg: Message, state: Arc<AppState>) -> Result<(), teloxide::RequestError> {
    let user_id = msg.from().map_or(UserId(0), |u| u.id);
    if !config::is_admin(user_id) {
        bot.send_message(msg.chat.id, "❌ This command is only available for administrators.")
            .reply_to_message_id(msg.id)
            .await?;
        return Ok(());
    }

    state.log_buffer.clear();

    let truncate_result = tokio::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(config::LOG_FILE)
        .await;

    match truncate_result {
        Ok(_) => {
            bot.send_message(msg.chat.id, "✅ <b>Logs cleared successfully!</b> Both in-memory buffer and log file were reset.")
                .reply_to_message_id(msg.id)
                .parse_mode(ParseMode::Html)
                .await?;
            log::info!("Log buffer and log file cleared by admin {}", user_id);
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            bot.send_message(msg.chat.id, "✅ <b>In-memory logs cleared.</b> (No log file on disk).")
                .reply_to_message_id(msg.id)
                .parse_mode(ParseMode::Html)
                .await?;
            log::info!("In-memory log buffer cleared by admin {}", user_id);
        }
        Err(e) => {
            bot.send_message(msg.chat.id, format!("⚠️ In-memory buffer cleared, but failed to truncate disk file: {}", e))
                .reply_to_message_id(msg.id)
                .await?;
            log::error!("Failed to truncate log file: {}", e);
        }
    }
     
    Ok(())
}
