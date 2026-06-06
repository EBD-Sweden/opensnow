use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use rand::RngCore;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::fmt;
use std::process::Command;
use std::sync::{Arc, Mutex};

use crate::contract::SecretHandleDescriptor;

/// Metadata-only secret provider configuration.
///
/// Enterprise providers identify where OpenSnow can resolve a secret from a
/// trusted execution path. They intentionally model provider handles/paths and
/// KMS/transit key IDs, never raw secret material.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "provider", rename_all = "snake_case")]
pub enum SecretProviderConfig {
    LocalDev {
        key_id: String,
    },
    AwsSecretsManager {
        handle_ref: String,
        kms_key_id: Option<String>,
    },
    GcpSecretManager {
        handle_ref: String,
        kms_key_id: Option<String>,
    },
    Vault {
        path: String,
        transit_key: Option<String>,
    },
}

impl SecretProviderConfig {
    pub fn local_dev(key_id: impl Into<String>) -> Self {
        Self::LocalDev {
            key_id: key_id.into(),
        }
    }

    pub fn aws_secrets_manager(handle_ref: impl Into<String>, kms_key_id: Option<&str>) -> Self {
        Self::AwsSecretsManager {
            handle_ref: handle_ref.into(),
            kms_key_id: kms_key_id.map(str::to_string),
        }
    }

    pub fn gcp_secret_manager(handle_ref: impl Into<String>, kms_key_id: Option<&str>) -> Self {
        Self::GcpSecretManager {
            handle_ref: handle_ref.into(),
            kms_key_id: kms_key_id.map(str::to_string),
        }
    }

    pub fn vault(path: impl Into<String>, transit_key: Option<&str>) -> Self {
        Self::Vault {
            path: path.into(),
            transit_key: transit_key.map(str::to_string),
        }
    }

    pub fn is_enterprise_backed(&self) -> bool {
        matches!(
            self,
            Self::AwsSecretsManager { .. } | Self::GcpSecretManager { .. } | Self::Vault { .. }
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SecretState {
    Active,
    Revoked,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretMetadata {
    pub descriptor: SecretHandleDescriptor,
    pub handle_id: String,
    pub provider: SecretProviderConfig,
    pub state: SecretState,
    pub version: u32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl fmt::Debug for SecretMetadata {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretMetadata")
            .field("descriptor", &self.descriptor)
            .field("handle_id", &self.handle_id)
            .field("provider", &self.provider)
            .field("state", &self.state)
            .field("version", &self.version)
            .field("created_at", &self.created_at)
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct SecretValue(String);

impl SecretValue {
    /// Only trusted internal call sites should use this accessor.
    ///
    /// The type does not implement Serialize and its Debug output is redacted so
    /// accidental API responses, audit records, and logs cannot leak the value.
    pub fn expose_to_trusted_execution_path(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretValue(<redacted>)")
    }
}

#[derive(Clone, PartialEq, Eq)]
enum ExternalSecretBackend {
    AwsSecretsManager,
    GcpSecretManager,
    Vault,
}

#[derive(Clone, PartialEq, Eq)]
pub struct ExternalSecretResolver {
    backend: ExternalSecretBackend,
    handle: String,
    vault_field: Option<String>,
    command_override: Option<OsString>,
}

impl fmt::Debug for ExternalSecretResolver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExternalSecretResolver")
            .field("provider", &self.provider_name())
            .field("handle", &redacted_handle(&self.handle))
            .field("vault_field", &self.vault_field)
            .finish_non_exhaustive()
    }
}

impl ExternalSecretResolver {
    pub fn from_handle(handle: &str) -> Result<Self> {
        let trimmed = handle.trim();
        if let Some(secret_id) = trimmed.strip_prefix("aws-secretsmanager://") {
            let secret_id = secret_id.trim();
            if secret_id.is_empty() {
                bail!("unsupported external secret handle: empty AWS Secrets Manager secret id");
            }
            return Ok(Self {
                backend: ExternalSecretBackend::AwsSecretsManager,
                handle: secret_id.to_string(),
                vault_field: None,
                command_override: None,
            });
        }
        if let Some(secret_ref) = trimmed.strip_prefix("gcp-secretmanager://") {
            let secret_ref = secret_ref.trim();
            if secret_ref.is_empty() {
                bail!("unsupported external secret handle: empty GCP Secret Manager secret ref");
            }
            return Ok(Self {
                backend: ExternalSecretBackend::GcpSecretManager,
                handle: secret_ref.to_string(),
                vault_field: None,
                command_override: None,
            });
        }
        if let Some(path_and_field) = trimmed.strip_prefix("vault://") {
            let (path, field) = path_and_field
                .split_once('#')
                .map(|(path, field)| (path, Some(field.to_string())))
                .unwrap_or((path_and_field, None));
            let path = path.trim().trim_start_matches('/');
            if path.is_empty() {
                bail!("unsupported external secret handle: empty Vault path");
            }
            return Ok(Self {
                backend: ExternalSecretBackend::Vault,
                handle: path.to_string(),
                vault_field: field,
                command_override: None,
            });
        }
        bail!(
            "unsupported external secret handle: expected aws-secretsmanager://, gcp-secretmanager://, or vault:// URI"
        )
    }

    pub fn provider_name(&self) -> &'static str {
        match self.backend {
            ExternalSecretBackend::AwsSecretsManager => "aws-secrets-manager",
            ExternalSecretBackend::GcpSecretManager => "gcp-secret-manager",
            ExternalSecretBackend::Vault => "vault",
        }
    }

    pub fn with_command_override(mut self, command: impl Into<OsString>) -> Self {
        self.command_override = Some(command.into());
        self
    }

    pub fn resolve(&self) -> Result<SecretValue> {
        match self.backend {
            ExternalSecretBackend::AwsSecretsManager => self.resolve_aws_cli(),
            ExternalSecretBackend::GcpSecretManager => self.resolve_gcp_cli(),
            ExternalSecretBackend::Vault => self.resolve_vault_cli(),
        }
    }

    fn resolve_aws_cli(&self) -> Result<SecretValue> {
        let command = self
            .command_override
            .clone()
            .unwrap_or_else(|| OsString::from("aws"));
        let output = Command::new(command)
            .args([
                "secretsmanager",
                "get-secret-value",
                "--secret-id",
                self.handle.as_str(),
                "--query",
                "SecretString",
                "--output",
                "text",
            ])
            .output()
            .with_context(|| {
                format!(
                    "aws-secrets-manager resolution failed closed before retrieving {}",
                    redacted_handle(&self.handle)
                )
            })?;
        if !output.status.success() {
            bail!(
                "aws-secrets-manager resolution failed closed for {}: exit status {}",
                redacted_handle(&self.handle),
                output.status
            );
        }
        secret_from_command_stdout(output.stdout, "aws-secrets-manager", &self.handle)
    }

    fn resolve_gcp_cli(&self) -> Result<SecretValue> {
        let command = self
            .command_override
            .clone()
            .unwrap_or_else(|| OsString::from("gcloud"));
        let output = Command::new(command)
            .args([
                "secrets",
                "versions",
                "access",
                "latest",
                "--secret",
                self.handle.as_str(),
            ])
            .output()
            .with_context(|| {
                format!(
                    "gcp-secret-manager resolution failed closed before retrieving {}",
                    redacted_handle(&self.handle)
                )
            })?;
        if !output.status.success() {
            bail!(
                "gcp-secret-manager resolution failed closed for {}: exit status {}",
                redacted_handle(&self.handle),
                output.status
            );
        }
        secret_from_command_stdout(output.stdout, "gcp-secret-manager", &self.handle)
    }

    fn resolve_vault_cli(&self) -> Result<SecretValue> {
        let command = self
            .command_override
            .clone()
            .unwrap_or_else(|| OsString::from("vault"));
        let field = self.vault_field.as_deref().unwrap_or("value");
        let output = Command::new(command)
            .args(["kv", "get", "-field", field, self.handle.as_str()])
            .output()
            .with_context(|| {
                format!(
                    "vault resolution failed closed before retrieving {}",
                    redacted_handle(&self.handle)
                )
            })?;
        if !output.status.success() {
            bail!(
                "vault resolution failed closed for {}: exit status {}",
                redacted_handle(&self.handle),
                output.status
            );
        }
        secret_from_command_stdout(output.stdout, "vault", &self.handle)
    }
}

fn secret_from_command_stdout(
    stdout: Vec<u8>,
    provider: &str,
    handle: &str,
) -> Result<SecretValue> {
    let value = String::from_utf8(stdout).with_context(|| {
        format!(
            "{provider} resolution failed closed for {}: non-UTF8 secret payload",
            redacted_handle(handle)
        )
    })?;
    let value = value.trim_end_matches(['\r', '\n']).to_string();
    if value.is_empty() || value == "None" || value == "null" {
        bail!(
            "{provider} resolution failed closed for {}: empty secret payload",
            redacted_handle(handle)
        );
    }
    Ok(SecretValue(value))
}

fn redacted_handle(handle: &str) -> String {
    let suffix: String = handle
        .chars()
        .rev()
        .take(8)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("<redacted:{}>", suffix)
}

pub trait SecretProvider: Send + Sync {
    fn create_secret(
        &self,
        descriptor: SecretHandleDescriptor,
        raw_secret: &str,
    ) -> Result<SecretMetadata>;
    fn list_secrets(&self, organization_id: &str) -> Result<Vec<SecretMetadata>>;
    fn rotate_secret(
        &self,
        organization_id: &str,
        handle_id: &str,
        raw_secret: &str,
    ) -> Result<SecretMetadata>;
    fn revoke_secret(&self, organization_id: &str, handle_id: &str) -> Result<SecretMetadata>;
    fn resolve_secret(&self, organization_id: &str, handle_id: &str) -> Result<SecretValue>;
}

/// Local-development sealed store backed by SQLite.
///
/// This is intentionally not the production boundary: enterprise deployments
/// must configure AWS/GCP/Vault handles. The local store still persists only a
/// sealed payload plus metadata, which gives tests and demos the same no-raw-
/// secret API shape as cloud-backed providers.
/// Versioned sealed-payload format tag for AES-256-GCM AEAD sealing.
///
/// Layout: `"v1." + hex(nonce[12]) + "." + hex(ciphertext||tag)`.
/// Payloads written by the prior (insecure) XOR keystream have no version
/// prefix (`hex(nonce[16]) + "." + hex(cipher)`) and are rejected by `unseal`
/// so legacy ciphertext is detectable and can be migrated/rotated.
const SEALED_PAYLOAD_V1: &str = "v1";

/// AES-256-GCM nonce length in bytes (96-bit nonce, the AEAD standard size).
const AES_GCM_NONCE_LEN: usize = 12;

pub struct TrustedSecretStore {
    conn: Arc<Mutex<Connection>>,
    provider: SecretProviderConfig,
    /// Derived 32-byte AES-256 data key. Derived deterministically from the
    /// configured master-key source via SHA-256 so arbitrary-length operator
    /// key material maps to a valid 256-bit key without weakening key handling.
    data_key: [u8; 32],
}

impl TrustedSecretStore {
    pub fn local_dev(conn: Arc<Mutex<Connection>>, master_key: &str) -> Result<Self> {
        if master_key.is_empty() {
            bail!("local secret store master key must not be empty");
        }
        let store = Self {
            conn,
            provider: SecretProviderConfig::local_dev("local-dev-sealed-store"),
            data_key: derive_data_key(master_key.as_bytes()),
        };
        store.init_schema()?;
        Ok(store)
    }

    fn init_schema(&self) -> Result<()> {
        let db = self.conn.lock().unwrap();
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS secret_handles (
                organization_id TEXT NOT NULL,
                handle_id TEXT NOT NULL,
                descriptor_json TEXT NOT NULL,
                provider_json TEXT NOT NULL,
                state TEXT NOT NULL,
                version INTEGER NOT NULL,
                sealed_payload TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (organization_id, handle_id)
            );",
        )
        .context("failed to create secret_handles table")?;
        Ok(())
    }

    /// Seal a secret with AES-256-GCM using a fresh random 96-bit nonce.
    ///
    /// The output is authenticated (GCM tag appended to the ciphertext) and
    /// versioned (`v1.<nonce>.<ciphertext||tag>`). The version tag is bound as
    /// AEAD associated data so a downgrade to a different format fails to
    /// authenticate. Returns an error only if the AEAD primitive itself fails,
    /// which is not expected for valid keys/nonces.
    fn seal(&self, raw_secret: &str) -> Result<String> {
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&self.data_key));
        let mut nonce_bytes = [0u8; AES_GCM_NONCE_LEN];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ciphertext = cipher
            .encrypt(
                nonce,
                Payload {
                    msg: raw_secret.as_bytes(),
                    aad: SEALED_PAYLOAD_V1.as_bytes(),
                },
            )
            .map_err(|_| anyhow!("failed to seal secret with AES-256-GCM"))?;
        Ok(format!(
            "{}.{}.{}",
            SEALED_PAYLOAD_V1,
            hex_encode(&nonce_bytes),
            hex_encode(&ciphertext)
        ))
    }

    /// Open a sealed payload, verifying authenticity (tamper/wrong-key both
    /// fail). Legacy unversioned XOR payloads are rejected with an actionable
    /// error so they can be re-sealed/rotated rather than silently trusted.
    fn unseal(&self, sealed_payload: &str) -> Result<String> {
        let mut parts = sealed_payload.splitn(3, '.');
        let version = parts
            .next()
            .ok_or_else(|| anyhow!("invalid sealed secret payload"))?;
        if version != SEALED_PAYLOAD_V1 {
            bail!(
                "sealed secret uses an unsupported or legacy format (expected '{SEALED_PAYLOAD_V1}'); \
                 rotate this handle to re-seal it with AES-256-GCM"
            );
        }
        let nonce_hex = parts
            .next()
            .ok_or_else(|| anyhow!("invalid sealed secret payload: missing nonce"))?;
        let cipher_hex = parts
            .next()
            .ok_or_else(|| anyhow!("invalid sealed secret payload: missing ciphertext"))?;
        let nonce_bytes = hex_decode(nonce_hex)?;
        if nonce_bytes.len() != AES_GCM_NONCE_LEN {
            bail!("invalid sealed secret payload: bad nonce length");
        }
        let ciphertext = hex_decode(cipher_hex)?;
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&self.data_key));
        let nonce = Nonce::from_slice(&nonce_bytes);
        let plain = cipher
            .decrypt(
                nonce,
                Payload {
                    msg: &ciphertext,
                    aad: SEALED_PAYLOAD_V1.as_bytes(),
                },
            )
            .map_err(|_| {
                anyhow!(
                    "failed to open sealed secret: authentication failed (tampered or wrong key)"
                )
            })?;
        String::from_utf8(plain).context("sealed secret payload is not UTF-8")
    }

    fn metadata_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SecretMetadata> {
        let descriptor_json: String = row.get(0)?;
        let provider_json: String = row.get(1)?;
        let state: String = row.get(2)?;
        let version: u32 = row.get::<_, i64>(3)? as u32;
        let created_at: String = row.get(4)?;
        let updated_at: String = row.get(5)?;
        let descriptor: SecretHandleDescriptor =
            serde_json::from_str(&descriptor_json).map_err(|err| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(err),
                )
            })?;
        let provider: SecretProviderConfig =
            serde_json::from_str(&provider_json).map_err(|err| {
                rusqlite::Error::FromSqlConversionFailure(
                    1,
                    rusqlite::types::Type::Text,
                    Box::new(err),
                )
            })?;
        let created_at = DateTime::parse_from_rfc3339(&created_at)
            .map_err(|err| {
                rusqlite::Error::FromSqlConversionFailure(
                    4,
                    rusqlite::types::Type::Text,
                    Box::new(err),
                )
            })?
            .with_timezone(&Utc);
        let updated_at = DateTime::parse_from_rfc3339(&updated_at)
            .map_err(|err| {
                rusqlite::Error::FromSqlConversionFailure(
                    5,
                    rusqlite::types::Type::Text,
                    Box::new(err),
                )
            })?
            .with_timezone(&Utc);
        let state = match state.as_str() {
            "active" => SecretState::Active,
            "revoked" => SecretState::Revoked,
            _ => SecretState::Revoked,
        };
        Ok(SecretMetadata {
            handle_id: descriptor.handle_id.clone(),
            descriptor,
            provider,
            state,
            version,
            created_at,
            updated_at,
        })
    }

    fn get_metadata(&self, organization_id: &str, handle_id: &str) -> Result<SecretMetadata> {
        let db = self.conn.lock().unwrap();
        db.query_row(
            "SELECT descriptor_json, provider_json, state, version, created_at, updated_at
             FROM secret_handles WHERE organization_id = ?1 AND handle_id = ?2",
            params![organization_id, handle_id],
            Self::metadata_from_row,
        )
        .with_context(|| format!("secret handle not found: {organization_id}/{handle_id}"))
    }
}

impl SecretProvider for TrustedSecretStore {
    fn create_secret(
        &self,
        descriptor: SecretHandleDescriptor,
        raw_secret: &str,
    ) -> Result<SecretMetadata> {
        if raw_secret.is_empty() {
            bail!("secret value must not be empty");
        }
        let now = Utc::now();
        let provider_json = serde_json::to_string(&self.provider)?;
        let descriptor_json = serde_json::to_string(&descriptor)?;
        let sealed_payload = self.seal(raw_secret)?;
        let db = self.conn.lock().unwrap();
        db.execute(
            "INSERT INTO secret_handles
             (organization_id, handle_id, descriptor_json, provider_json, state, version, sealed_payload, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, 'active', 1, ?5, ?6, ?7)",
            params![
                &descriptor.organization_id,
                &descriptor.handle_id,
                &descriptor_json,
                &provider_json,
                &sealed_payload,
                &now.to_rfc3339(),
                &now.to_rfc3339()
            ],
        )
        .context("failed to create sealed secret handle")?;
        drop(db);
        self.get_metadata(&descriptor.organization_id, &descriptor.handle_id)
    }

    fn list_secrets(&self, organization_id: &str) -> Result<Vec<SecretMetadata>> {
        let db = self.conn.lock().unwrap();
        let mut stmt = db.prepare(
            "SELECT descriptor_json, provider_json, state, version, created_at, updated_at
             FROM secret_handles WHERE organization_id = ?1 ORDER BY handle_id",
        )?;
        let rows = stmt.query_map(params![organization_id], Self::metadata_from_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .context("failed to list sealed secret handles")
    }

    fn rotate_secret(
        &self,
        organization_id: &str,
        handle_id: &str,
        raw_secret: &str,
    ) -> Result<SecretMetadata> {
        if raw_secret.is_empty() {
            bail!("secret value must not be empty");
        }
        let existing = self.get_metadata(organization_id, handle_id)?;
        if existing.state != SecretState::Active {
            bail!("cannot rotate revoked secret handle");
        }
        let sealed_payload = self.seal(raw_secret)?;
        let updated_at = Utc::now().to_rfc3339();
        let db = self.conn.lock().unwrap();
        db.execute(
            "UPDATE secret_handles
             SET sealed_payload = ?1, version = version + 1, updated_at = ?2
             WHERE organization_id = ?3 AND handle_id = ?4 AND state = 'active'",
            params![sealed_payload, updated_at, organization_id, handle_id],
        )
        .context("failed to rotate sealed secret handle")?;
        drop(db);
        self.get_metadata(organization_id, handle_id)
    }

    fn revoke_secret(&self, organization_id: &str, handle_id: &str) -> Result<SecretMetadata> {
        let updated_at = Utc::now().to_rfc3339();
        let db = self.conn.lock().unwrap();
        let updated = db.execute(
            "UPDATE secret_handles SET state = 'revoked', updated_at = ?1
             WHERE organization_id = ?2 AND handle_id = ?3",
            params![updated_at, organization_id, handle_id],
        )?;
        if updated == 0 {
            bail!("secret handle not found: {organization_id}/{handle_id}");
        }
        drop(db);
        self.get_metadata(organization_id, handle_id)
    }

    fn resolve_secret(&self, organization_id: &str, handle_id: &str) -> Result<SecretValue> {
        let db = self.conn.lock().unwrap();
        let row: Option<(String, String)> = db
            .query_row(
                "SELECT state, sealed_payload FROM secret_handles
                 WHERE organization_id = ?1 AND handle_id = ?2",
                params![organization_id, handle_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let (state, sealed_payload) =
            row.ok_or_else(|| anyhow!("secret handle not found: {organization_id}/{handle_id}"))?;
        if state != "active" {
            bail!("secret handle is revoked: {organization_id}/{handle_id}");
        }
        drop(db);
        Ok(SecretValue(self.unseal(&sealed_payload)?))
    }
}

/// Derive a fixed 32-byte AES-256 data key from arbitrary-length master key
/// material. SHA-256 maps the operator-supplied key source to a valid 256-bit
/// key deterministically; it does not weaken key handling (the master key is
/// still the sole secret) and gives stable round-trips for a given master key.
fn derive_data_key(master_key: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"opensnow-sealed-secret-store/v1");
    hasher.update(master_key);
    hasher.finalize().into()
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn hex_decode(input: &str) -> Result<Vec<u8>> {
    if !input.len().is_multiple_of(2) {
        bail!("invalid hex length");
    }
    (0..input.len())
        .step_by(2)
        .map(|idx| {
            u8::from_str_radix(&input[idx..idx + 2], 16).context("invalid hex in sealed payload")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn new_store(master_key: &str) -> TrustedSecretStore {
        let conn = Arc::new(Mutex::new(Connection::open_in_memory().expect("sqlite")));
        TrustedSecretStore::local_dev(conn, master_key).expect("store")
    }

    #[test]
    fn aead_seal_unseal_round_trips() {
        let store = new_store("unit-test-master-key");
        let sealed = store.seal("super-secret-value").expect("seal");
        assert!(
            sealed.starts_with("v1."),
            "payload must be versioned: {sealed}"
        );
        assert!(
            !sealed.contains("super-secret-value"),
            "plaintext must not appear in sealed payload"
        );
        let opened = store.unseal(&sealed).expect("unseal");
        assert_eq!(opened, "super-secret-value");
    }

    #[test]
    fn aead_uses_fresh_nonce_per_seal() {
        let store = new_store("unit-test-master-key");
        let a = store.seal("same-plaintext").expect("seal a");
        let b = store.seal("same-plaintext").expect("seal b");
        assert_ne!(a, b, "random nonce must make ciphertexts differ");
        assert_eq!(store.unseal(&a).unwrap(), store.unseal(&b).unwrap());
    }

    #[test]
    fn aead_detects_tampered_ciphertext() {
        let store = new_store("unit-test-master-key");
        let sealed = store.seal("tamper-me").expect("seal");
        // Flip the last hex nibble of the ciphertext+tag.
        let mut bytes: Vec<char> = sealed.chars().collect();
        let last = bytes.len() - 1;
        bytes[last] = if bytes[last] == 'a' { 'b' } else { 'a' };
        let tampered: String = bytes.into_iter().collect();
        let err = store.unseal(&tampered).unwrap_err().to_string();
        assert!(err.contains("authentication failed"), "got: {err}");
    }

    #[test]
    fn aead_fails_with_wrong_key() {
        let sealer = new_store("master-key-one");
        let sealed = sealer.seal("cross-key-secret").expect("seal");
        let other = new_store("master-key-two");
        let err = other.unseal(&sealed).unwrap_err().to_string();
        assert!(err.contains("authentication failed"), "got: {err}");
    }

    #[test]
    fn aead_rejects_legacy_unversioned_payload() {
        let store = new_store("unit-test-master-key");
        // Simulate the prior XOR format: "<nonce_hex>.<cipher_hex>" with no version tag.
        let legacy = format!("{}.{}", hex_encode(&[0u8; 16]), hex_encode(b"whatever"));
        let err = store.unseal(&legacy).unwrap_err().to_string();
        assert!(err.contains("legacy format"), "got: {err}");
    }

    #[cfg(unix)]
    fn executable_script(body: &str) -> tempfile::TempPath {
        use std::os::unix::fs::PermissionsExt;

        let file = tempfile::NamedTempFile::new().expect("temp script");
        fs::write(file.path(), body).expect("write script");
        let mut perms = fs::metadata(file.path())
            .expect("script metadata")
            .permissions();
        perms.set_mode(0o700);
        fs::set_permissions(file.path(), perms).expect("chmod script");
        file.into_temp_path()
    }

    #[cfg(unix)]
    #[test]
    fn external_secret_resolver_supports_gcp_secret_manager_without_leaking_handles() {
        let script = executable_script("#!/bin/sh\nprintf '%s' 'resolved-gcp-secret'\n");

        let resolver = ExternalSecretResolver::from_handle(
            "gcp-secretmanager://projects/acme/secrets/oidc-client-secret/versions/latest",
        )
        .expect("gcp secretmanager handle should parse")
        .with_command_override(script.as_os_str());

        assert_eq!(resolver.provider_name(), "gcp-secret-manager");
        assert!(format!("{resolver:?}").contains("<redacted:"));
        assert!(!format!("{resolver:?}").contains("oidc-client-secret"));
        assert_eq!(
            resolver
                .resolve()
                .expect("fake gcloud command should resolve")
                .expose_to_trusted_execution_path(),
            "resolved-gcp-secret"
        );
    }

    #[cfg(unix)]
    #[test]
    fn external_secret_resolver_fails_closed_for_empty_or_failed_cloud_payloads() {
        let empty_script = executable_script("#!/bin/sh\nprintf '%s' ''\n");
        let failed_script = executable_script("#!/bin/sh\nexit 42\n");

        let empty = ExternalSecretResolver::from_handle("aws-secretsmanager://prod/db/password")
            .expect("aws handle should parse")
            .with_command_override(empty_script.as_os_str());
        assert!(
            empty
                .resolve()
                .unwrap_err()
                .to_string()
                .contains("empty secret payload")
        );

        let failed = ExternalSecretResolver::from_handle("vault://secret/data/opensnow#password")
            .expect("vault handle should parse")
            .with_command_override(failed_script.as_os_str());
        assert!(
            failed
                .resolve()
                .unwrap_err()
                .to_string()
                .contains("failed closed")
        );
    }
}
