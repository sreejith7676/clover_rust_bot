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
    #[command(description = "Restart the bot (Admin only).")]
    Reboot,
    #[command(description = "View bot logs (Admin only). Usage: /log [lines]")]
    Log(String),
    #[command(description = "Clear bot logs (Admin only)")]
    Clearlogs,
}

#[tokio::main]
async fn main() {
    // Set up log rotation in current directory using config::LOG_FILE
    let file_appender = tracing_appender::rolling::never(".", config::LOG_FILE);
    let (non_blocking_appender, _guard) = tracing_appender::non_blocking(file_appender);

    let filter = EnvFilter::new("clover_bot_rust=info,teloxide=off,reqwest=off,hyper=off,h2=off,tungstenite=off");

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_writer(std::io::stdout))
        .with(fmt::layer().with_writer(non_blocking_appender).with_ansi(false))
        .init();

    log::info!("=== CLOVER BOT STARTING (Rust Version) ===");

    dotenvy::dotenv().expect("Failed to load .env file");

    let bot = Bot::from_env();

    // 1. Load the custom statuses from JSON file
    let saved_statuses = helpers::storage::load_statuses();
    log::info!("Loaded {} custom device statuses from disk.", saved_statuses.len());

    // 2. Initialize AppState using the new constructor
    let initial_state = AppState::new(saved_statuses);

    // 3. Preload filenames from GitHub
    match helpers::github::preload_devices(&initial_state).await {
        Ok(count) => log::info!("Preloaded {} devices successfully", count),
        Err(e) => log::error!("Failed to preload devices: {}", e),
    }

    let handler = Update::filter_message()
        .branch(dptree::entry().filter_command::<Command>().endpoint(handlers::command_handler));
        
    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![Arc::new(initial_state)])
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;

    log::info!("Bot is shutting down.");
}
