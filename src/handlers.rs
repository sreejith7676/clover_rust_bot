use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::sync::Arc;
use teloxide::{
    prelude::*,
    types::{InlineKeyboardButton, InlineKeyboardMarkup, InputFile, ParseMode},
};
use reqwest::Url;
use sysinfo::{System, Disks};
use std::time::Instant;

use crate::{config, helpers, state::AppState, Command};

// This line reads your image file and embeds it into the bot's executable
const BANNER_BYTES: &[u8] = include_bytes!("../assets/banner.png");

pub async fn command_handler(
    bot: Bot,
    msg: Message,
    cmd: Command,
    state: Arc<AppState>,
) -> Result<(), teloxide::RequestError> {
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
        // Pass the raw string args to clover_info to handle subcommands
        Command::Clover(args) => clover_info(bot, msg, args, state).await?,
        Command::List => list_devices(bot, msg, state).await?,
        Command::Maintainership => maintainership(bot, msg).await?,
        Command::Uptime => uptime(bot, msg, state).await?,
        Command::Ping => ping(bot, msg).await?,
        Command::Stats => stats_command(bot, msg, state).await?,
        Command::Server => server(bot, msg).await?,
        Command::Reboot => reboot(bot, msg).await?,
        Command::Log(lines) => log_cmd(bot, msg, lines).await?,
        Command::Clearlogs => clearlogs(bot, msg).await?,
    };

    Ok(())
}

async fn start(bot: Bot, msg: Message) -> Result<(), teloxide::RequestError> {
    let mut welcome = "🤖 <b>Welcome to the Official Clover ROM Bot!</b>\n\n".to_string();
    welcome.push_str("📱 <b>Device Information:</b>\n");
    welcome.push_str("• /clover &lt;codename&gt; - Get device information\n");
    welcome.push_str("• /list - List all supported devices\n\n");
    welcome.push_str("👥 <b>Maintainership:</b>\n");
    welcome.push_str("• /maintainership - Apply for maintainership\n");

    let is_special_user =
        msg.chat.id == config::SPECIAL_GROUP || config::is_admin(msg.from().unwrap().id);

    if is_special_user {
        welcome.push_str("\n⚙️ <b>Server Commands:</b>\n");
        welcome.push_str("• /uptime - Bot uptime\n");
        welcome.push_str("• /ping - Bot response time\n");
        welcome.push_str("• /stats - Bot usage stats\n");
        welcome.push_str("• /server - Server details\n");
    }

    if config::is_admin(msg.from().unwrap().id) {
        welcome.push_str("\n🔧 <b>Admin Commands:</b>\n");
        welcome.push_str("• /reboot - Restart the bot\n");
        welcome.push_str("• /log - View bot logs\n");
        welcome.push_str("• /clearlogs - Clear bot logs\n");
        // FIX: Use &lt; and &gt; instead of < and >
        welcome.push_str("• /clover insert &lt;device&gt; &lt;status&gt; - Set status\n");
        welcome.push_str("• /clover remove &lt;device&gt; - Remove status\n");
    }

    bot.send_message(msg.chat.id, welcome).reply_to_message_id(msg.id).parse_mode(ParseMode::Html).await?;
    Ok(())
}

async fn clover_info(
    bot: Bot,
    msg: Message,
    raw_args: String,
    state: Arc<AppState>,
) -> Result<(), teloxide::RequestError> {
    let args: Vec<&str> = raw_args.split_whitespace().collect();
    let command = args.get(0).copied().unwrap_or("").to_lowercase();
    
    // Correctly get UserId without crashing
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
        let status = args[2..].join(" ").replace("\"", "");

        let mut statuses = state.device_statuses.write().await;
        statuses.insert(codename.clone(), status.clone());
        // Save to file
        crate::helpers::storage::save_statuses(&statuses);

        bot.send_message(msg.chat.id, format!("✅ Set status for <b>{}</b> to <b>{}</b>", codename, status))
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
            list_text.push_str(&format!("• <b>{}</b>: {}\n", code, stat));
        }

        bot.send_message(msg.chat.id, list_text).parse_mode(ParseMode::Html).reply_to_message_id(msg.id).await?;
        return Ok(());
    }

    // --- 4. STANDARD FETCH LOGIC ---
    let codename = raw_args; // Treat input as codename if not a command

    if codename.is_empty() {
        bot.send_message(msg.chat.id, "❌ Please specify a device codename.\nExample: <code>/clover peridot</code>")
            .reply_to_message_id(msg.id).parse_mode(ParseMode::Html).await?;
        return Ok(());
    }

    // A. Send "Fetching..." message
    let status_msg = bot.send_message(msg.chat.id, format!("🔍 <b>Fetching '{}'...</b>", codename))
        .reply_to_message_id(msg.id)
        .parse_mode(ParseMode::Html)
        .await?;

    // B. Perform the fetch
    let fetch_result = helpers::github::load_device_from_github(&codename, &state).await;

    match fetch_result {
        Ok(Some(device)) => {
            let mut stats = state.stats.write().await;
            stats.devices_queried += 1;
            drop(stats);

            let status_emoji = if device.status == "Active" || device.status == "Stable" { "🟢" } else { "🔴" };
            let status_with_emoji = format!("{} {}", status_emoji, device.status);

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
                🧑‍💻 <b>Maintainer:</b> {}",
                device.device_name,
                device.codename,
                status_with_emoji,
                device.version,
                device.android_version,
                device.date,
                device.size,
                device.security_patch,
                device.md5,
                device.maintainer
            );

            let download_url = Url::parse(&device.download_url).unwrap_or_else(|_| Url::parse("https://thecloverproject.com").unwrap());
            let website_url_str = format!("https://thecloverproject.com/download/{}", device.codename.to_lowercase());
            let website_url = Url::parse(&website_url_str).unwrap_or_else(|_| Url::parse("https://thecloverproject.com").unwrap());

            let keyboard = InlineKeyboardMarkup::new(vec![
                vec![InlineKeyboardButton::url("📥 Download Now", download_url)],
                vec![
                    InlineKeyboardButton::url("🌐 Website", website_url),
                ],
            ]);

            let banner = InputFile::memory(BANNER_BYTES).file_name("banner.png");

            // C. SEND PHOTO FIRST
            bot.send_photo(msg.chat.id, banner)
                .caption(caption_text)
                .reply_to_message_id(msg.id)
                .parse_mode(ParseMode::Html)
                .reply_markup(keyboard)
                .await?;

            // D. DELETE "FETCHING" TEXT AFTER SUCCESS
            let _ = bot.delete_message(msg.chat.id, status_msg.id).await;
        }
        Ok(None) => {
            // If no data, delete fetching then show error
            let _ = bot.delete_message(msg.chat.id, status_msg.id).await;
            bot.send_message(msg.chat.id, format!(
                    "❌ No information for <code>{}</code>\nUse /list to see available devices.",
                    codename
                ),
            )
            .reply_to_message_id(msg.id)
            .parse_mode(ParseMode::Html)
            .await?;
        }
        Err(e) => {
            // If error, delete fetching then show error
            let _ = bot.delete_message(msg.chat.id, status_msg.id).await;
            log::error!("Error fetching device info for {}: {}", codename, e);
            bot.send_message(msg.chat.id, "❌ An error occurred while fetching device data.")
                .reply_to_message_id(msg.id)
                .await?;
        }
    }
    Ok(())
}

async fn list_devices(
    bot: Bot,
    msg: Message,
    state: Arc<AppState>,
) -> Result<(), teloxide::RequestError> {
    // 1. Read from cache
    let devices = state.devices_cache.read().await;
    
    if devices.is_empty() {
        bot.send_message(msg.chat.id, "❌ Device list is empty or loading... Try again in a moment.")
            .reply_to_message_id(msg.id)
            .await?;
        return Ok(());
    }

    // 2. Group devices by Brand (with Normalization)
    let mut grouped: std::collections::HashMap<String, Vec<&crate::models::DeviceSummary>> = std::collections::HashMap::new();

    for device in devices.iter() {
        let raw_brand = device.brand.to_lowercase();

        let display_brand = if raw_brand.contains("xiaomi") 
            || raw_brand.contains("redmi") 
            || raw_brand.contains("poco") 
            || raw_brand.contains("mi ") 
            || raw_brand == "mi" {
            "Xiaomi / Redmi / Poco".to_string()
        } else {
            device.brand.clone()
        };

        grouped
            .entry(display_brand)
            .or_insert_with(Vec::new)
            .push(device);
    }

    // 3. Custom Sorting Logic
    let mut brands: Vec<String> = grouped.keys().cloned().collect();
    
    // Define priority: 0 is highest (top of list)
    let get_priority = |b: &str| -> i32 {
        if b.contains("Xiaomi") { 0 }
        else if b.contains("Motorola") { 1 }
        else if b.contains("Realme") { 2 }
        else if b.contains("Nothing") { 3 }
        else { 4 } // Everything else comes last alphabetically
    };

    brands.sort_by(|a, b| {
        let prio_a = get_priority(a);
        let prio_b = get_priority(b);
        if prio_a != prio_b {
            prio_a.cmp(&prio_b) // Sort by priority first
        } else {
            a.cmp(b) // Then alphabetically for brands with same priority
        }
    });

    let mut msg_text = "📋 <b>List of Supported Devices:</b>\n\n".to_string();

    // 4. Build the message using the custom sorted order
    for brand in brands {
        if let Some(dev_list) = grouped.get(&brand) {
            msg_text.push_str(&format!("<b>{}</b>\n", brand));
            
            // Sort devices inside the brand by name
            let mut sorted_devs = dev_list.clone();
            sorted_devs.sort_by(|a, b| a.name.cmp(&b.name));

            for dev in sorted_devs {
                msg_text.push_str(&format!("• {} (<code>{}</code>)\n", dev.name, dev.codename));
            }
            msg_text.push_str("\n");
        }
    }

    msg_text.push_str("Use /clover &lt;codename&gt; to get details.");

    bot.send_message(msg.chat.id, msg_text)
        .reply_to_message_id(msg.id)
        .parse_mode(ParseMode::Html)
        .await?;
        
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
    sys.refresh_all();
     
    let disks = Disks::new_with_refreshed_list();
    let disk = &disks[0]; 

    let cpu = sys.global_cpu_info();
    let ram_total = sys.total_memory();
    let ram_used = sys.used_memory();

    let text = format!(
        "🖥 <b>Server Status</b>\n━━━━━━━━━━━━━━━━━━━━━━\n\
        📟 <b>System:</b> {} {}\n\
        💻 <b>CPU Usage:</b> {:.2}%\n\
        🧠 <b>RAM Usage:</b> {:.2}% ({}MB / {}MB)\n\
        💾 <b>Disk Usage:</b> {:.2}% ({}GB / {}GB)\n\
        ⏱ <b>Uptime:</b> {}",
        System::name().unwrap_or_default(), System::os_version().unwrap_or_default(),
        cpu.cpu_usage(),
        (ram_used as f64 / ram_total as f64) * 100.0, ram_used / 1024 / 1024, ram_total / 1024 / 1024,
        (disk.total_space() - disk.available_space()) as f64 / disk.total_space() as f64 * 100.0,
        (disk.total_space() - disk.available_space()) / 1024 / 1024 / 1024,
        disk.total_space() / 1024 / 1024 / 1024,
        helpers::formatting::format_uptime(System::uptime())
    );

    bot.send_message(msg.chat.id, text).reply_to_message_id(msg.id).parse_mode(ParseMode::Html).await?;
    Ok(())
}

async fn reboot(bot: Bot, msg: Message) -> Result<(), teloxide::RequestError> {
    if !config::is_admin(msg.from().unwrap().id) {
        bot.send_message(msg.chat.id, "❌ This command is only available for administrators.")
            .reply_to_message_id(msg.id)
            .await?;
        return Ok(());
    }

    bot.send_message(msg.chat.id, "🔄 <b>Rebooting bot...</b>")
        .reply_to_message_id(msg.id)
        .parse_mode(ParseMode::Html)
        .await?;
    log::warn!(
        "Reboot command initiated by admin {}",
        msg.from().unwrap().id
    );

    use std::process::exit;
    exit(1);
}

fn get_logs(lines_to_read: usize) -> Result<String, std::io::Error> {
    match File::open(config::LOG_FILE) {
        Ok(file) => {
            let reader = BufReader::new(file);
            let lines: Vec<String> = reader.lines().filter_map(Result::ok).collect();
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

async fn log_cmd(bot: Bot, msg: Message, args: String) -> Result<(), teloxide::RequestError> {
    if !config::is_admin(msg.from().unwrap().id) {
        bot.send_message(msg.chat.id, "❌ This command is only available for administrators.")
            .reply_to_message_id(msg.id)
            .await?;
        return Ok(());
    }

    let lines = args.split_whitespace().next()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(50)
        .clamp(10, 200);

    match get_logs(lines) {
        Ok(mut logs) => {
            if logs.is_empty() {
                logs = "Log file is empty or has not been created yet.".to_string();
            }
             
            if logs.len() > 4000 {
                let start_index = logs.len() - 4000;
                logs = format!("... (logs truncated, showing last 4000 characters) ...\n{}", &logs[start_index..]);
            }
             
            let text = format!("📝 <b>Bot Logs (last {} lines):</b>\n<pre>{}</pre>", lines, teloxide::utils::html::escape(&logs));
            bot.send_message(msg.chat.id, text).reply_to_message_id(msg.id).parse_mode(ParseMode::Html).await?;
        }
        Err(e) => {
            log::error!("Failed to read logs: {}", e);
            bot.send_message(msg.chat.id, format!("❌ Error reading logs: {}. Make sure the bot has run long enough to create a log file.", e))
                .reply_to_message_id(msg.id).await?;
        }
    }
     
    Ok(())
}

async fn clearlogs(bot: Bot, msg: Message) -> Result<(), teloxide::RequestError> {
    if !config::is_admin(msg.from().unwrap().id) {
        bot.send_message(msg.chat.id, "❌ This command is only available for administrators.")
            .reply_to_message_id(msg.id)
            .await?;
        return Ok(());
    }

    match tokio::fs::remove_file(config::LOG_FILE).await {
        Ok(_) => {
            bot.send_message(msg.chat.id, "✅ Log file cleared successfully. New logs will be created automatically.")
                .reply_to_message_id(msg.id)
                .await?;
            log::info!("Log file cleared by admin {}", msg.from().unwrap().id);
        }
        Err(e) => {
            if e.kind() == std::io::ErrorKind::NotFound {
                bot.send_message(msg.chat.id, "ℹ️ No log file found to clear.")
                    .reply_to_message_id(msg.id)
                    .await?;
            } else {
                bot.send_message(msg.chat.id, format!("❌ Failed to clear log file: {}", e))
                    .reply_to_message_id(msg.id)
                    .await?;
                log::error!("Failed to clear log file: {}", e);
            }
        }
    }
     
    Ok(())
}
