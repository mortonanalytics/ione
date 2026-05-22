use aes_gcm::{
    aead::{Aead, KeyInit, OsRng},
    Aes256Gcm, Nonce,
};
use anyhow::{Context, Result};
use base64::{engine::general_purpose, Engine};
use rand::RngCore;

const NONCE_LEN: usize = 12;
pub const TOKEN_KEY_VERSION_CURRENT: u8 = 0x01;

pub fn encrypt_token(token: &str) -> Result<Vec<u8>> {
    let cipher = cipher_from_env()?;
    let mut nonce = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce);
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), token.as_bytes())
        .map_err(|_| anyhow::anyhow!("encrypt token"))?;
    let mut out = Vec::with_capacity(NONCE_LEN + ciphertext.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

pub fn decrypt_token(ciphertext: &[u8]) -> Result<String> {
    anyhow::ensure!(
        ciphertext.len() > NONCE_LEN,
        "encrypted token payload is too short"
    );
    let cipher = cipher_from_env()?;
    let (nonce, body) = ciphertext.split_at(NONCE_LEN);
    let plaintext = cipher
        .decrypt(Nonce::from_slice(nonce), body)
        .map_err(|_| anyhow::anyhow!("decrypt token"))?;
    String::from_utf8(plaintext).context("token plaintext is not utf-8")
}

pub fn encrypt_webhook_secret(secret: &[u8]) -> Result<Vec<u8>> {
    let cipher = webhook_cipher_from_env()?;
    let mut nonce = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce);
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), secret)
        .map_err(|_| anyhow::anyhow!("encrypt webhook secret"))?;
    let mut out = Vec::with_capacity(1 + NONCE_LEN + ciphertext.len());
    out.push(TOKEN_KEY_VERSION_CURRENT);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

pub fn decrypt_webhook_secret(ciphertext: &[u8]) -> Result<String> {
    anyhow::ensure!(
        ciphertext.len() > 1 + NONCE_LEN,
        "encrypted webhook secret payload is too short"
    );
    anyhow::ensure!(
        ciphertext[0] == TOKEN_KEY_VERSION_CURRENT,
        "unsupported webhook secret key version"
    );
    let cipher = webhook_cipher_from_env()?;
    let (nonce, body) = ciphertext[1..].split_at(NONCE_LEN);
    let plaintext = cipher
        .decrypt(Nonce::from_slice(nonce), body)
        .map_err(|_| anyhow::anyhow!("decrypt webhook secret"))?;
    String::from_utf8(plaintext).context("webhook secret plaintext is not utf-8")
}

pub fn encrypt_versioned(plaintext: &[u8]) -> Result<Vec<u8>> {
    let cipher = cipher_from_env()?;
    let mut nonce = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce);
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), plaintext)
        .map_err(|_| anyhow::anyhow!("encrypt token"))?;
    let mut out = Vec::with_capacity(1 + NONCE_LEN + ciphertext.len());
    out.push(TOKEN_KEY_VERSION_CURRENT);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

pub fn decrypt_versioned(ciphertext: &[u8]) -> Result<Vec<u8>> {
    anyhow::ensure!(
        ciphertext.len() > 1 + NONCE_LEN,
        "encrypted token payload is too short"
    );
    anyhow::ensure!(
        ciphertext[0] == TOKEN_KEY_VERSION_CURRENT,
        "unsupported token key version"
    );
    let cipher = cipher_from_env()?;
    let (nonce, body) = ciphertext[1..].split_at(NONCE_LEN);
    cipher
        .decrypt(Nonce::from_slice(nonce), body)
        .map_err(|_| anyhow::anyhow!("decrypt token"))
}

pub fn validate_env_key() -> Result<()> {
    let _ = cipher_from_env()?;
    Ok(())
}

pub fn validate_webhook_secret_key() -> Result<()> {
    let _ = webhook_cipher_from_env()?;
    Ok(())
}

fn cipher_from_env() -> Result<Aes256Gcm> {
    let raw = std::env::var("IONE_TOKEN_KEY")
        .context("IONE_TOKEN_KEY must be set to encrypt peer OAuth tokens")?;
    cipher_from_raw_key(&raw, "IONE_TOKEN_KEY")
}

fn webhook_cipher_from_env() -> Result<Aes256Gcm> {
    if let Ok(raw) = std::env::var("IONE_WEBHOOK_SECRET_KEY") {
        return cipher_from_raw_key(&raw, "IONE_WEBHOOK_SECRET_KEY");
    }
    let dev_mode = std::env::var("IONE_DEV_MODE")
        .map(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false);
    anyhow::ensure!(
        dev_mode || cfg!(test),
        "IONE_WEBHOOK_SECRET_KEY must be set to encrypt inbound webhook signing secrets"
    );
    tracing::warn!(
        "IONE_WEBHOOK_SECRET_KEY is not set; falling back to IONE_TOKEN_KEY for webhook secrets in dev/test"
    );
    let raw = std::env::var("IONE_TOKEN_KEY")
        .context("IONE_TOKEN_KEY must be set when webhook secret key falls back in dev/test")?;
    cipher_from_raw_key(&raw, "IONE_TOKEN_KEY")
}

fn cipher_from_raw_key(raw: &str, name: &str) -> Result<Aes256Gcm> {
    let key = general_purpose::STANDARD
        .decode(raw)
        .or_else(|_| hex::decode(raw))
        .with_context(|| format!("{name} must be base64 or hex encoded"))?;
    anyhow::ensure!(key.len() == 32, "{name} must decode to 32 bytes");
    Ok(Aes256Gcm::new_from_slice(&key).expect("validated key length"))
}
