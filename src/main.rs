use std::io::IsTerminal;
use std::process::ExitCode;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use teloxide::{prelude::*, types::BotCommand, utils::command::BotCommands};
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

mod config;
mod handlers;
mod helpers;
mod models;
mod state;

use state::AppState;

/// Default log filter; override with the `RUST_LOG` environment variable.
/// Note: teloxide's error handler is left enabled so failing command handlers are visible.
const DEFAULT_LOG_FILTER: &str = "clover_bot_rust=debug,teloxide=info,teloxide::dispatching::dispatcher=error,reqwest=off,hyper=off,hyper_util=off,h2=off,rustls=off,tungstenite=off";

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

/// Commands shown in Telegram's command menu for everyone (server/admin commands stay hidden).
fn public_commands() -> Vec<BotCommand> {
    const PUBLIC: [&str; 8] = [
        "start",
        "clover",
        "list",
        "maintainership",
        "changelog",
        "flash",
        "install",
        "maintainer",
    ];
    Command::bot_commands()
        .into_iter()
        // The derive yields "/start"; Telegram's setMyCommands wants the bare name.
        .map(|c| BotCommand::new(c.command.trim_start_matches('/').to_string(), c.description))
        .filter(|c| PUBLIC.contains(&c.command.as_str()))
        .collect()
}

#[tokio::main]
async fn main() -> ExitCode {
    // Load .env first so RUST_LOG and the bot settings can live there.
    let dotenv_result = dotenvy::dotenv();

    // Thread-safe in-memory circular log buffer + non-blocking disk logging.
    let log_buffer = helpers::logs::LogBuffer::new(1000);
    let file_appender = tracing_appender::rolling::never(".", config::LOG_FILE);
    let (non_blocking_appender, _guard) = tracing_appender::non_blocking(file_appender);

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_LOG_FILTER));

    tracing_subscriber::registry()
        .with(filter)
        .with(
            fmt::layer()
                .with_writer(std::io::stdout)
                .with_ansi(std::io::stdout().is_terminal()),
        )
        .with(fmt::layer().with_writer(non_blocking_appender).with_ansi(false))
        .with(fmt::layer().with_writer(log_buffer.clone()).with_ansi(false))
        .init();

    log::info!("=== CLOVER BOT STARTING (Rust Version) ===");
    if let Err(e) = dotenv_result {
        log::debug!("Notice: .env file not loaded ({}). Relying on system environment variables.", e);
    }

    // `_guard` is dropped when `main` returns, which flushes the file logger.
    match run(log_buffer).await {
        Ok(true) => {
            log::warn!("Exiting for restart (reboot requested).");
            ExitCode::from(1)
        }
        Ok(false) => {
            log::info!("Bot is shutting down.");
            ExitCode::SUCCESS
        }
        Err(e) => {
            log::error!("Fatal error: {:#}", e);
            ExitCode::from(2)
        }
    }
}

/// Runs the bot until shutdown. Returns `true` if a reboot was requested.
async fn run(log_buffer: helpers::logs::LogBuffer) -> anyhow::Result<bool> {
    let config = config::Config::from_env().context("invalid configuration")?;
    let token = std::env::var("TELOXIDE_TOKEN").context("TELOXIDE_TOKEN is not set")?;
    let bot = Bot::new(token);

    // Spawn periodic log file size safety guard (truncates if > 15MB)
    helpers::logs::spawn_log_size_guard();

    // Verify Telegram API connection with retry loop (prevents crashing if offline during startup)
    let mut retry_delay = Duration::from_secs(2);
    loop {
        match bot.get_me().send().await {
            Ok(me) => {
                log::info!("Connected to Telegram as @{} ({})", me.username(), me.id);
                break;
            }
            Err(e) => {
                log::warn!("Unable to reach Telegram API ({}); retrying in {:?}...", e, retry_delay);
                tokio::time::sleep(retry_delay).await;
                retry_delay = std::cmp::min(retry_delay * 2, Duration::from_secs(30));
            }
        }
    }

    if let Err(e) = bot.set_my_commands(public_commands()).await {
        log::warn!("Could not register the bot command menu: {}", e);
    }

    // Load custom statuses, then build the shared state.
    let saved_statuses = helpers::storage::load_statuses();
    log::info!("Loaded {} custom device statuses from disk.", saved_statuses.len());
    let state = Arc::new(AppState::new(config, saved_statuses, log_buffer));

    // Preload the device catalog; the background task retries/refreshes it afterwards.
    match helpers::github::preload_devices(&state).await {
        Ok(count) => log::info!("Preloaded {} devices successfully", count),
        Err(e) => log::error!("Failed to preload devices (will retry in the background): {:#}", e),
    }
    helpers::github::spawn_catalog_refresh(state.clone());

    let listener_error_handler = Arc::new(helpers::logs::ListenerErrorHandler::new());
    let notify_handler = listener_error_handler.clone();

    let handler = dptree::entry()
        .inspect(move || notify_handler.notify_success())
        .branch(
            Update::filter_message()
                .filter_command::<Command>()
                .endpoint(handlers::command_handler),
        )
        .branch(Update::filter_callback_query().endpoint(handlers::callback_handler));

    let listener = teloxide::update_listeners::polling_default(bot.clone()).await;

    let mut dispatcher = Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![state.clone()])
        .default_handler(|_| async {})
        .enable_ctrlc_handler()
        .build();

    // Graceful shutdown on /reboot and SIGTERM (Ctrl-C is handled by the dispatcher itself).
    let shutdown_token = dispatcher.shutdown_token();
    let signal_state = state.clone();
    tokio::spawn(async move {
        wait_for_shutdown_request(&signal_state).await;
        match shutdown_token.shutdown() {
            Ok(done) => done.await,
            Err(_) => log::debug!("Dispatcher was already idle during shutdown."),
        }
    });

    dispatcher
        .dispatch_with_listener(listener, listener_error_handler)
        .await;

    Ok(state.reboot_requested.load(Ordering::SeqCst))
}

async fn wait_for_shutdown_request(state: &AppState) {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = state.shutdown.notified() => {}
                    _ = term.recv() => log::info!("SIGTERM received; shutting down gracefully."),
                }
                return;
            }
            Err(e) => log::warn!("Could not install SIGTERM handler: {}", e),
        }
    }
    state.shutdown.notified().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_menu_hides_admin_and_server_commands() {
        let cmds = public_commands();
        let names: Vec<String> = cmds.iter().map(|c| c.command.clone()).collect();
        assert_eq!(cmds.len(), 8, "{names:?}");
        for hidden in ["sync", "reload", "reboot", "log", "clearlogs", "server", "stats", "uptime", "ping"] {
            assert!(!names.iter().any(|n| n.trim_start_matches('/') == hidden), "{hidden} leaked");
        }
        assert!(names.iter().all(|n| !n.starts_with('/')), "Telegram wants names without a slash: {names:?}");
    }
}
