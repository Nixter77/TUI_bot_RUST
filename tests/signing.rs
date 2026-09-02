//! Drive shipped HMAC signing with the official Binance test vector.

use std::collections::BTreeMap;
use tui_bot::signing::{canonical_query, form_encode, sign_query, signed_query_string};

#[test]
fn official_binance_hmac_vector() {
    let secret = "NhqPtmdSJYdKjVHjA7PZj4Mge3R5YNiP1e3UZjInClVN65XAbvqqM6A7H5fATj0j";
    let query = "symbol=LTCBTC&side=BUY&type=LIMIT&timeInForce=GTC&quantity=1&price=0.1&recvWindow=5000&timestamp=1499827319559";
    let sig = sign_query(secret, query).unwrap();
    assert_eq!(
        sig,
        "c8db56825ae71d6d79447849e617115f4a920fa2acdcab2b053c4b2838bd6b71"
    );
}

#[test]
fn signed_query_appends_signature_without_embedding_secret() {
    let secret = "super-secret-value-not-a-real-key";
    let mut params = BTreeMap::new();
    params.insert("symbol".into(), "BTCUSDT".into());
    params.insert("side".into(), "BUY".into());
    let q = signed_query_string(&params, secret, 1_499_827_319_559, 5000).unwrap();
    assert!(q.contains("signature="));
    assert!(!q.contains(secret));
    let mut expected = params.clone();
    expected.insert("timestamp".into(), "1499827319559".into());
    expected.insert("recvWindow".into(), "5000".into());
    assert!(q.starts_with(&canonical_query(&expected)));
}

#[test]
fn empty_secret_rejected() {
    assert!(sign_query("", "a=1").is_err());
}

#[test]
fn form_encode_percent_encodes_ampersand() {
    assert_eq!(form_encode("BTCUSDT"), "BTCUSDT");
    assert_eq!(form_encode("a&b"), "a%26b");
}
