//! Binance USDT-M TestNet TUI trader (Rust port of TUI_bot).

pub mod app;
pub mod backtest;
pub mod config;
pub mod continuation;
pub mod dayrisk;
pub mod engine;
pub mod errorlog;
pub mod errors;
pub mod exchange;
pub mod flatten;
pub mod indicators;
pub mod journal;
pub mod keys;
pub mod live;
pub mod models;
pub mod money;
pub mod profit;
pub mod ranking;
pub mod render;
pub mod report;
pub mod scalp;
pub mod sessions;
pub mod signals;
pub mod signing;
pub mod sim;
pub mod snapshot;
pub mod trail;
pub mod trend;
pub mod tui;
pub mod view;

pub use app::{main_with_env, parse_args, run, CliArgs};
pub use config::{load_config, Config, ConfigError, DEFAULT_TESTNET_BASE, MAINNET_BASE};
pub use engine::{decide, momentum_decision, tick, tick_decisions, MomentumParams, STRATEGY_NAMES};
pub use models::{Decision, EngineState, MarketSnapshot};
