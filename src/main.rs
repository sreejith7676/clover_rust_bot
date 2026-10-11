use std::sync::Arc;

use teloxide::{prelude::*, utils::command::BotCommands};
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

mod config;
mod handlers;
mod helpers;
mod models;
mod state;

use state::AppState;

#[derive(BotCommands, Clone, Debug)]
#[command(
    rename_rule = "lowercase",
    description = "These commands are supported:"
)]
pub enum Command {
    #[command(description = "Display this text.")]
    Start,
    #[command(description = "Get device information. Usage: /clover <codename>")]
    Clover(String),
    #[command(description = "List all supported devices.")]
    List,
    #[command(description = "Apply for maintainership.")]
    Maintainership,
    #[command(description = "Check bot uptime.")]
    Uptime,
    #[command(description = "Check bot response time.")]
    Ping,
    #[command(description = "Show bot usage statistics.")]
    Stats,
    #[command(description = "Show server details.")]
    Server,
    #[command(description = "Get device changelog. Usage: /changelog <codename>")]
    Changelog(String),
    #[command(description = "Get device flashing guide. Usage: /flash <codename>")]
    Flash(String),
    #[command(description = "Alias for /flash. Usage: /install <codename>")]
    Install(String),
    #[command(description = "Search or list maintainers. Usage: /maintainer [name]")]
    Maintainer(String),
    #[command(description = "Resync device catalog from GitHub (Admin only).")]
    Sync,
    #[command(description = "Alias for /sync. Resync device catalog (Admin only).")]
    Reload,
    #[command(description = "Restart the bot (Admin only).")]
    Reboot,
    #[command(description = "View bot logs (Admin only). Usage: /log [lines|file]")]
    Log(String),
    #[command(description = "Clear bot logs (Admin only)")]
    Clearlogs,
}

#[tokio::main]
async fn main() {
    // 1. Initialize thread-safe in-memory circular log buffer
    let log_buffer = helpers::logs::LogBuffer::new(1000);

    // 2. Set up non-blocking disk file logging using config::LOG_FILE
    let file_appender = tracing_appender::rolling::never(".", config::LOG_FILE);
    let (non_blocking_appender, _guard) = tracing_appender::non_blocking(file_appender);

    let filter = EnvFilter::new(
        "clover_bot_rust=debug,teloxide=info,teloxide::dispatching::dispatcher=error,teloxide::error_handlers=off,reqwest=off,hyper=off,h2=off,tungstenite=off",
    );

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_writer(std::io::stdout))
        .with(fmt::layer().with_writer(non_blocking_appender).with_ansi(false))
        .with(fmt::layer().with_writer(log_buffer.clone()).with_ansi(false))
        .init();

    log::info!("=== CLOVER BOT STARTING (Rust Version) ===");

    // Spawn periodic log file size safety guard (truncates if > 15MB)
    helpers::logs::spawn_log_size_guard();

    // Gracefully load .env if available, without panicking if missing
    if let Err(e) = dotenvy::dotenv() {
        log::debug!("Notice: .env file not loaded ({}). Relying on system environment variables.", e);
    }

    let bot = Bot::from_env();

    // Verify Telegram API connection with retry loop (prevents crashing if offline during startup)
    let mut retry_delay = std::time::Duration::from_secs(2);
    loop {
        match bot.get_me().send().await {
            Ok(me) => {
                log::info!("Connected to Telegram as @{} ({})", me.username(), me.id);
                break;
            }
            Err(e) => {
                log::warn!(
                    "Unable to reach Telegram API ({}); retrying in {:?}...",
                    e,
                    retry_delay
                );
                tokio::time::sleep(retry_delay).await;
                retry_delay = std::cmp::min(retry_delay * 2, std::time::Duration::from_secs(30));
            }
        }
    }

    // 3. Load custom statuses from JSON file
    let saved_statuses = helpers::storage::load_statuses();
    log::info!("Loaded {} custom device statuses from disk.", saved_statuses.len());

    // 4. Initialize ListenerErrorHandler and AppState
    let listener_error_handler = Arc::new(helpers::logs::ListenerErrorHandler::new());
    let initial_state = AppState::new(saved_statuses, log_buffer, listener_error_handler.clone());

    // 5. Preload device catalog and metadata from GitHub
    match helpers::github::preload_devices(&initial_state).await {
        Ok(count) => log::info!("Preloaded {} devices successfully", count),
        Err(e) => log::error!("Failed to preload devices: {}", e),
    }

    let handler = dptree::entry()
        .inspect(|state: Arc<AppState>| {
            state.listener_error_handler.notify_success();
        })
        .branch(
            Update::filter_message()
                .branch(dptree::entry().filter_command::<Command>().endpoint(handlers::command_handler))
        )
        .branch(
            Update::filter_callback_query()
                .endpoint(handlers::callback_handler)
        );

    let listener = teloxide::dispatching::update_listeners::polling_default(bot.clone()).await;

    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![Arc::new(initial_state)])
        .default_handler(|_| async {})
        .enable_ctrlc_handler()
        .build()
        .dispatch_with_listener(listener, listener_error_handler)
        .await;

    log::info!("Bot is shutting down.");
}
