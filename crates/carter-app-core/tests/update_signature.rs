//! 更新包验签的回归测试。
//!
//! 夹具是一次性**测试密钥**签的（不是发布密钥）：
//!
//! ```text
//! tauri signer generate -w /tmp/test-key -p ""      # 只为生成这一组夹具
//! printf 'hello carter updater\n' > payload.bin
//! TAURI_SIGNING_PRIVATE_KEY="$(cat /tmp/test-key)" \
//!   tauri signer sign --app-version 0.2.0 payload.bin
//! ```
//!
//! 以后 Tauri 换签名格式时，这条测试会先炸，而不是等用户的更新装不上。

use carter_app_core::net::verify_update;

const PAYLOAD: &[u8] = include_bytes!("fixtures/update-payload.bin");
const SIG: &str = include_str!("fixtures/update-payload.bin.sig");
const PUBKEY: &str = include_str!("fixtures/test-pubkey.txt");

#[test]
fn a_genuine_signature_verifies_for_the_signed_version() {
    verify_update(PUBKEY, PAYLOAD, SIG, "0.2.0").expect("真签名要能验过");
    // 清单里的版本可能带 `v`。
    verify_update(PUBKEY, PAYLOAD, SIG, "v0.2.0").expect("带 v 的版本号也认");
}

#[test]
fn a_tampered_payload_is_rejected() {
    let mut bytes = PAYLOAD.to_vec();
    bytes[0] ^= 0xff;
    let err = verify_update(PUBKEY, &bytes, SIG, "0.2.0").unwrap_err();
    assert!(err.contains("签名校验失败"), "{err}");
}

/// 拿一份合法签名的旧包、改一下清单里的版本号 —— 插件默认配置下这是会装上的，
/// 所以 `verify_update` 必须自己挡。
#[test]
fn a_version_mismatch_is_rejected() {
    let err = verify_update(PUBKEY, PAYLOAD, SIG, "0.3.0").unwrap_err();
    assert!(err.contains("签名里的版本是 0.2.0"), "{err}");
}

#[test]
fn garbage_inputs_are_errors_not_panics() {
    assert!(verify_update(PUBKEY, PAYLOAD, "not a signature", "0.2.0").is_err());
    assert!(verify_update("not a key", PAYLOAD, SIG, "0.2.0").is_err());
}
