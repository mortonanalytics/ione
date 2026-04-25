use aes_gcm::{
    aead::{Aead, KeyInit, OsRng},
    Aes256Gcm, Nonce,
};
use anyhow::{Context, Result};
use base64::{engine::general_purpose, Engine};
use rand::RngCore;

const NONCE_LEN: usize = 12;

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

pub fn validate_env_key() -> Result<()> {
    let _ = cipher_from_env()?;
    Ok(())
}

fn cipher_from_env() -> Result<Aes256Gcm> {
    let raw = std::env::var("IONE_TOKEN_KEY")
        .context("IONE_TOKEN_KEY must be set to encrypt peer OAuth tokens")?;
    let key = general_purpose::STANDARD
        .decode(&raw)
        .or_else(|_| hex::decode(&raw))
        .context("IONE_TOKEN_KEY must be base64 or hex encoded")?;
    anyhow::ensure!(key.len() == 32, "IONE_TOKEN_KEY must decode to 32 bytes");
    Ok(Aes256Gcm::new_from_slice(&key).expect("validated key length"))
}
