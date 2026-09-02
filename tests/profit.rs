//! Drive shipped account_profit / current_equity / pinned baseline.

use rust_decimal::Decimal;
use tui_bot::profit::{
    account_profit, current_equity, load_persisted_starting_equity, persist_starting_equity, pin_starting_equity,
    EquityPin,
};

#[test]
fn profit_is_equity_minus_start() {
    let profit = account_profit(Decimal::from(10500), Decimal::new(2505, 1), Decimal::from(10000));
    assert_eq!(profit, Decimal::new(7505, 1));
    assert_eq!(
        profit,
        current_equity(Decimal::from(10500), Decimal::new(2505, 1)) - Decimal::from(10000)
    );
}

#[test]
fn loss_when_equity_below_start() {
    let profit = account_profit(Decimal::from(9000), Decimal::from(-100), Decimal::from(10000));
    assert_eq!(profit, Decimal::from(-1100));
}

#[test]
fn zero_when_flat() {
    assert_eq!(
        account_profit(Decimal::from(1000), Decimal::ZERO, Decimal::from(1000)),
        Decimal::ZERO
    );
}

#[test]
fn rebasing_every_poll_hides_real_profit() {
    let first_equity = current_equity("3039.6780".parse().unwrap(), "93.0573".parse().unwrap());
    let later_wallet: Decimal = "3039.8808".parse().unwrap();
    let later_upnl: Decimal = "93.9810".parse().unwrap();
    let rebased = pin_starting_equity(None, current_equity(later_wallet, later_upnl));
    assert_eq!(account_profit(later_wallet, later_upnl, rebased), Decimal::ZERO);
    let pinned = pin_starting_equity(Some(first_equity), current_equity(later_wallet, later_upnl));
    assert_eq!(pinned, first_equity);
    assert!(account_profit(later_wallet, later_upnl, pinned) > Decimal::ONE);
}

#[test]
fn persist_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("starting_equity");
    persist_starting_equity("3132.7353".parse().unwrap(), Some(&path));
    assert_eq!(
        load_persisted_starting_equity(Some(&path)),
        Some("3132.7353".parse().unwrap())
    );
    assert_eq!(load_persisted_starting_equity(Some(&tmp.path().join("missing"))), None);
}

#[test]
fn equity_pin_captures_once() {
    let mut pin = EquityPin {
        value: None,
        persist: false,
    };
    let first = pin.capture(Decimal::from(1000));
    let again = pin.capture(Decimal::from(1100));
    assert_eq!(first, Decimal::from(1000));
    assert_eq!(again, Decimal::from(1000));
    let mut env = EquityPin::from_config(Some(Decimal::from(5000)));
    assert_eq!(env.capture(Decimal::ONE), Decimal::from(5000));
    assert!(!env.persist);
}

#[test]
fn persist_and_load_serialize_on_same_file() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("starting_equity");
    let w = path.clone();
    let r = path.clone();
    let writer = std::thread::spawn(move || {
        for i in 0..40 {
            persist_starting_equity(Decimal::from(1000 + i), Some(&w));
        }
    });
    let reader = std::thread::spawn(move || {
        for _ in 0..40 {
            let _ = load_persisted_starting_equity(Some(&r));
        }
    });
    writer.join().unwrap();
    reader.join().unwrap();
    let v = load_persisted_starting_equity(Some(&path)).unwrap();
    assert!(v >= Decimal::from(1000));
    assert!(v <= Decimal::from(1039));
}
