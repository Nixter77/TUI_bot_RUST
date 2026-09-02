//! CLI / TUI entry point. No TTY → print first frame and exit 0.

use crate::config::{load_config, Config, ConfigError};
use crate::dayrisk::apply_day_risk;
use crate::engine::select_strategy;
use crate::journal::{TradeJournal, DEFAULT_JOURNAL_PATH};
use crate::models::EngineState;
use crate::models::MarketSnapshot;
use crate::monitor::{build_monitor, render_monitor};
use crate::profit::{current_equity, EquityPin};
use crate::render::render_frame;
use crate::snapshot::{pull_snapshot, make_client};
use crate::view::build_view;
use clap::Parser;
use std::collections::HashMap;
use std::io::{self, IsTerminal, Write};
use std::path::Path;

#[derive(Debug, Parser, Clone)]
#[command(name = "tui-bot", about = "Binance USDT-M futures TestNet TUI trader")]
pub struct CliArgs {
    /// print the first frame to stdout and exit (default when stdout is not a TTY)
    #[arg(long = "dump-frame")]
    pub dump_frame: bool,
    /// send real TestNet orders (requires BINANCE_API_KEY and BINANCE_API_SECRET)
    #[arg(long)]
    pub live: bool,
    /// initial strategy: 1 momentum, 2 scalp, 3 trend+stop, 4 liquid continuation
    #[arg(long, default_value = "1", value_parser = clap::builder::PossibleValuesParser::new(["1", "2", "3", "4"]))]
    pub strategy: String,
    /// do not call the network; render an empty snapshot
    #[arg(long)]
    pub offline: bool,
    /// walk public USDT-M klines and print a profitability report (no orders)
    #[arg(long)]
    pub backtest: bool,
    /// print a summary of .state/trades.jsonl and .state/errors.jsonl
    #[arg(long)]
    pub report: bool,
    /// watch-only radar: waiting names, 24h tape, open/closed P&L (never sends orders)
    #[arg(long)]
    pub monitor: bool,
}

pub fn parse_args<I, S>(argv: I) -> Result<CliArgs, clap::Error>
where
    I: IntoIterator<Item = S>,
    S: Into<std::ffi::OsString>,
{
    CliArgs::try_parse_from(std::iter::once(std::ffi::OsString::from("tui-bot")).chain(argv.into_iter().map(Into::into)))
}

pub fn render_startup_frame(
    cfg: Option<&Config>,
    snapshot: Option<&MarketSnapshot>,
    strategy_id: i32,
    live: bool,
    offline: bool,
    environ: Option<&HashMap<String, String>>,
) -> Result<String, ConfigError> {
    let owned = if cfg.is_none() {
        Some(load_config(live, None, environ)?)
    } else {
        None
    };
    let cfg = cfg.unwrap_or_else(|| owned.as_ref().unwrap());
    let sid = select_strategy(strategy_id).map_err(ConfigError)?;
    let mut state = EngineState::new(sid);
    let owned_snap;
    let snap = if let Some(s) = snapshot {
        s
    } else {
        let mut client = if offline { None } else { Some(make_client(cfg)) };
        let mut pin = EquityPin::from_config(cfg.starting_equity);
        owned_snap = pull_snapshot(
            cfg,
            client.as_mut().map(|c| c as &mut dyn crate::exchange::SnapshotClient),
            &mut state,
            &mut pin,
            offline,
            None,
        );
        &owned_snap
    };
    Ok(render_frame(&build_view(cfg, &state, snap, "—", false)))
}

pub fn render_monitor_startup(
    cfg: Option<&Config>,
    strategy_id: i32,
    offline: bool,
    environ: Option<&HashMap<String, String>>,
) -> Result<String, ConfigError> {
    let owned = if cfg.is_none() {
        Some(load_config(false, None, environ)?)
    } else {
        None
    };
    let cfg = cfg.unwrap_or_else(|| owned.as_ref().unwrap());
    let sid = select_strategy(strategy_id).map_err(ConfigError)?;
    let mut state = EngineState::new(sid);
    crate::journal::seed_cooldowns(&mut state, crate::sessions::unix_now(), crate::errors::COOLDOWN_SEC);
    let mut client = if offline { None } else { Some(make_client(cfg)) };
    let mut pin = EquityPin::from_config(cfg.starting_equity);
    let snapshot = pull_snapshot(
        cfg,
        client.as_mut().map(|c| c as &mut dyn crate::exchange::SnapshotClient),
        &mut state,
        &mut pin,
        offline,
        None,
    );
    let now = crate::sessions::unix_now();
    let equity = current_equity(snapshot.account.wallet_balance, snapshot.account.unrealized_pnl);
    apply_day_risk(
        &mut state,
        now,
        equity,
        cfg.daily_loss_usdt,
        cfg.daily_loss_r,
        cfg.risk_pct,
    );
    let events = TradeJournal::new(Some(Path::new(DEFAULT_JOURNAL_PATH))).read_events();
    Ok(render_monitor(&build_monitor(cfg, &state, &snapshot, &events, now)))
}

pub fn run(
    args: &CliArgs,
    environ: Option<&HashMap<String, String>>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> i32 {
    let strategy = match args.strategy.parse::<i32>().map_err(|_| "strategy must be 1, 2, 3, or 4".to_string()).and_then(select_strategy) {
        Ok(s) => s,
        Err(e) => {
            let _ = writeln!(stderr, "config error: {e}");
            return 2;
        }
    };
    // `--monitor` never sends orders and does not require live keys.
    let want_live = args.live && !args.monitor;
    let cfg = match load_config(want_live, None, environ) {
        Ok(c) => c,
        Err(e) => {
            let _ = writeln!(stderr, "config error: {e}");
            return 2;
        }
    };

    if args.report {
        return crate::report::run_cli();
    }
    if args.backtest {
        return crate::backtest::run_cli();
    }

    let dump = args.dump_frame || !io::stdout().is_terminal();
    if args.monitor {
        if dump {
            match render_monitor_startup(Some(&cfg), strategy, args.offline, environ) {
                Ok(frame) => {
                    let _ = write!(stdout, "{frame}");
                }
                Err(e) => {
                    if let Ok(frame) = render_monitor_startup(Some(&cfg), strategy, true, environ) {
                        let _ = write!(stdout, "{frame}");
                    }
                    let _ = writeln!(stderr, "(render fallback after {e})");
                }
            }
            return 0;
        }
        let mut state = EngineState::new(strategy);
        return match crate::tui::run_monitor(&cfg, &mut state, args.offline) {
            Ok(()) => 0,
            Err(e) => {
                let _ = writeln!(stderr, "tui error: {e}");
                1
            }
        };
    }

    if dump {
        match render_startup_frame(Some(&cfg), None, strategy, args.live, args.offline, environ) {
            Ok(frame) => {
                let _ = write!(stdout, "{frame}");
            }
            Err(e) => {
                if let Ok(frame) = render_startup_frame(Some(&cfg), None, strategy, args.live, true, environ) {
                    let _ = write!(stdout, "{frame}");
                }
                let _ = writeln!(stderr, "(render fallback after {e})");
            }
        }
        return 0;
    }

    let mut state = EngineState::new(strategy);
    match crate::tui::run_tui(&cfg, &mut state, args.offline) {
        Ok(()) => 0,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            let _ = writeln!(stderr, "{e}");
            2
        }
        Err(e) => {
            let _ = writeln!(stderr, "tui error: {e}");
            1
        }
    }
}

pub fn main_with_env(argv: &[String], environ: Option<&HashMap<String, String>>) -> i32 {
    match parse_args(argv) {
        Ok(args) => {
            let mut out = io::stdout();
            let mut err = io::stderr();
            run(&args, environ, &mut out, &mut err)
        }
        Err(e) => {
            let _ = e.print();
            if e.use_stderr() {
                2
            } else {
                0
            }
        }
    }
}

pub fn main_with_args(argv: &[String]) -> i32 {
    main_with_env(argv, None)
}

/// Test helper: dump-frame with a fully isolated empty environ (no process keys).
pub fn dump_frame_offline_isolated() -> (i32, String, String) {
    dump_frame_offline_strategy("1")
}

pub fn dump_frame_offline_strategy(strategy: &str) -> (i32, String, String) {
    let args = CliArgs {
        dump_frame: true,
        live: false,
        strategy: strategy.into(),
        offline: true,
        backtest: false,
        report: false,
        monitor: false,
    };
    let env = HashMap::new();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = run(&args, Some(&env), &mut out, &mut err);
    (
        code,
        String::from_utf8_lossy(&out).into_owned(),
        String::from_utf8_lossy(&err).into_owned(),
    )
}

pub fn dump_monitor_offline_strategy(strategy: &str) -> (i32, String, String) {
    let args = CliArgs {
        dump_frame: true,
        live: false,
        strategy: strategy.into(),
        offline: true,
        backtest: false,
        report: false,
        monitor: true,
    };
    let env = HashMap::new();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = run(&args, Some(&env), &mut out, &mut err);
    (
        code,
        String::from_utf8_lossy(&out).into_owned(),
        String::from_utf8_lossy(&err).into_owned(),
    )
}

pub fn live_without_keys_isolated() -> (i32, String) {
    let args = CliArgs {
        dump_frame: true,
        live: true,
        strategy: "1".into(),
        offline: true,
        backtest: false,
        report: false,
        monitor: false,
    };
    let env = HashMap::new();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = run(&args, Some(&env), &mut out, &mut err);
    (code, String::from_utf8_lossy(&err).into_owned() + &String::from_utf8_lossy(&out))
}

pub fn help_text() -> String {
    CliArgs::command_help()
}

trait CommandHelp {
    fn command_help() -> String;
}

impl CommandHelp for CliArgs {
    fn command_help() -> String {
        use clap::CommandFactory;
        let mut cmd = CliArgs::command();
        let mut buf = Vec::new();
        cmd.write_help(&mut buf).ok();
        String::from_utf8_lossy(&buf).into_owned()
    }
}

pub fn report_on_paths(state_dir: &Path) -> (i32, String) {
    let trades = state_dir.join("trades.jsonl");
    let errors = state_dir.join("errors.jsonl");
    let text = crate::report::format_report(Some(&trades), Some(&errors));
    (0, text)
}
