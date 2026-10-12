# Clover Rust Bot

Telegram bot (teloxide) for The Clover Project: device info, changelogs, flashing guides and maintainer lookup, backed by the `official_updates` GitHub repo.

## Run

```sh
cp .env.example .env   # fill in token, ADMIN_IDS, ALLOWED_GROUP_IDS
cargo run --release
```

Requires Rust 1.91+. The bot reads/writes `device_status.json` and `clover_bot.log` in its **working directory**, so set `WorkingDirectory=` when running under systemd.

## Restarts

`/reboot` shuts the dispatcher down gracefully, flushes the log and exits with status `1`.
Run the bot under a supervisor that restarts it, e.g. systemd with `Restart=always` (or `on-failure`).

## Tests

```sh
cargo test                    # offline unit tests
cargo test -- --ignored       # also hit the live GitHub data
```

## Updating dependencies

`Cargo.lock` was resolved for Rust 1.91. On a newer toolchain run `cargo update`
(`sysinfo` 0.39+ needs Rust 1.95).
