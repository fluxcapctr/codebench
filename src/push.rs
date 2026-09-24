//! Web Push to paired phones: "a task needs you" even when the app is
//! closed. Payloads are encrypted for the phone, and signed with a VAPID
//! key kept in Codebench's config folder.

use crate::store::config_dir;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use web_push::{
    ContentEncoding, IsahcWebPushClient, SubscriptionInfo, Urgency, VapidSignatureBuilder, WebPushClient, WebPushError,
    WebPushMessageBuilder,
};

/// What the phone's browser gives us when it subscribes.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Subscription {
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
}

fn key_file() -> std::path::PathBuf {
    config_dir().join("vapid.pem")
}

/// Makes the VAPID key on first use: a P-256 key from openssl, readable by
/// you only.
fn ensure_key() -> Result<Vec<u8>, String> {
    use std::os::unix::fs::PermissionsExt;
    let file = key_file();
    if !file.is_file() {
        let _ = std::fs::create_dir_all(config_dir());
        let out = std::process::Command::new("openssl")
            .args(["ecparam", "-genkey", "-name", "prime256v1", "-noout", "-out"])
            .arg(&file)
            .output()
            .map_err(|e| format!("openssl: {e}"))?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
        }
        let _ = std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::read(&file).map_err(|e| e.to_string())
}

/// The public key a phone subscribes with, base64url.
pub fn public_key() -> Result<String, String> {
    let pem = ensure_key()?;
    let partial = VapidSignatureBuilder::from_pem_no_sub(pem.as_slice()).map_err(|e| e.to_string())?;
    Ok(URL_SAFE_NO_PAD.encode(partial.get_public_key()))
}

pub enum Sent {
    Ok,
    /// The phone unsubscribed or the subscription expired; forget it.
    Gone,
    Failed(String),
}

pub async fn send(sub: &Subscription, payload: &serde_json::Value) -> Sent {
    let pem = match ensure_key() {
        Ok(p) => p,
        Err(e) => return Sent::Failed(e),
    };
    let info = SubscriptionInfo::new(sub.endpoint.as_str(), sub.p256dh.as_str(), sub.auth.as_str());
    let sig = match VapidSignatureBuilder::from_pem(pem.as_slice(), &info).and_then(|mut b| {
        b.add_claim("sub", "https://github.com/fluxcapctr/codebench");
        b.build()
    }) {
        Ok(s) => s,
        Err(e) => return Sent::Failed(e.to_string()),
    };
    let body = payload.to_string();
    let mut msg = WebPushMessageBuilder::new(&info);
    msg.set_payload(ContentEncoding::Aes128Gcm, body.as_bytes());
    msg.set_vapid_signature(sig);
    msg.set_ttl(3600);
    msg.set_urgency(Urgency::High);
    let msg = match msg.build() {
        Ok(m) => m,
        Err(e) => return Sent::Failed(e.to_string()),
    };
    let client = match IsahcWebPushClient::new() {
        Ok(c) => c,
        Err(e) => return Sent::Failed(e.to_string()),
    };
    match client.send(msg).await {
        Ok(()) => Sent::Ok,
        Err(WebPushError::EndpointNotValid(_) | WebPushError::EndpointNotFound(_)) => Sent::Gone,
        Err(e) => Sent::Failed(e.to_string()),
    }
}
