use super::*;
use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead as _, KeyInit as _},
};
use anyhow::{Context as _, anyhow};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use cloud_api_types::GitHubConnectedAccount;
use rand::Rng as _;
use sha2::{Digest as _, Sha256};

impl Database {
    pub async fn upsert_github_integration(
        &self,
        user_id: UserId,
        login: String,
        scopes: Vec<String>,
        access_token: String,
        encryption_secret: &str,
    ) -> Result<()> {
        let encrypted_access_token = encrypt_access_token(&access_token, encryption_secret)?;
        let scopes_json = serde_json::to_string(&scopes)?;

        self.transaction(|tx| {
            let login = login.clone();
            let scopes_json = scopes_json.clone();
            let encrypted_access_token = encrypted_access_token.clone();
            async move {
                let now = chrono::Utc::now().naive_utc();
                github_integration::Entity::insert(github_integration::ActiveModel {
                    user_id: ActiveValue::Set(user_id),
                    login: ActiveValue::Set(login),
                    scopes_json: ActiveValue::Set(scopes_json),
                    encrypted_access_token: ActiveValue::Set(encrypted_access_token),
                    created_at: ActiveValue::Set(now.into()),
                    updated_at: ActiveValue::Set(now.into()),
                })
                .on_conflict(
                    OnConflict::column(github_integration::Column::UserId)
                        .update_columns([
                            github_integration::Column::Login,
                            github_integration::Column::ScopesJson,
                            github_integration::Column::EncryptedAccessToken,
                            github_integration::Column::UpdatedAt,
                        ])
                        .to_owned(),
                )
                .exec_without_returning(&*tx)
                .await?;

                Ok(())
            }
        })
        .await
    }

    pub async fn get_github_integration(
        &self,
        user_id: UserId,
        encryption_secret: &str,
    ) -> Result<Option<GitHubConnectedAccount>> {
        self.transaction(|tx| async move {
            let Some(row) = github_integration::Entity::find_by_id(user_id)
                .one(&*tx)
                .await?
            else {
                return Ok(None);
            };

            let access_token =
                decrypt_access_token(&row.encrypted_access_token, encryption_secret)?;
            let scopes = serde_json::from_str(&row.scopes_json)
                .context("failed to parse GitHub integration scopes")?;
            Ok(Some(GitHubConnectedAccount {
                login: row.login,
                scopes,
                access_token,
            }))
        })
        .await
    }

    pub async fn delete_github_integration(&self, user_id: UserId) -> Result<()> {
        self.transaction(|tx| async move {
            github_integration::Entity::delete_by_id(user_id)
                .exec(&*tx)
                .await?;
            Ok(())
        })
        .await
    }

    #[cfg(feature = "test-support")]
    pub async fn get_encrypted_github_access_token_for_test(
        &self,
        user_id: UserId,
    ) -> Result<Option<String>> {
        self.transaction(|tx| async move {
            Ok(github_integration::Entity::find_by_id(user_id)
                .one(&*tx)
                .await?
                .map(|row| row.encrypted_access_token))
        })
        .await
    }
}

fn encrypt_access_token(access_token: &str, encryption_secret: &str) -> anyhow::Result<String> {
    let cipher = Aes256Gcm::new_from_slice(&encryption_key(encryption_secret))
        .map_err(|_| anyhow!("invalid GitHub token encryption key"))?;
    let mut nonce = [0; 12];
    rand::rng().fill(&mut nonce);
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), access_token.as_bytes())
        .map_err(|_| anyhow!("failed to encrypt GitHub access token"))?;

    Ok(format!(
        "{}:{}",
        STANDARD.encode(nonce),
        STANDARD.encode(ciphertext)
    ))
}

fn decrypt_access_token(
    encrypted_access_token: &str,
    encryption_secret: &str,
) -> anyhow::Result<String> {
    let (nonce, ciphertext) = encrypted_access_token
        .split_once(':')
        .context("invalid encrypted GitHub access token")?;
    let nonce = STANDARD
        .decode(nonce)
        .context("invalid GitHub token nonce")?;
    let ciphertext = STANDARD
        .decode(ciphertext)
        .context("invalid GitHub token ciphertext")?;
    let cipher = Aes256Gcm::new_from_slice(&encryption_key(encryption_secret))
        .map_err(|_| anyhow!("invalid GitHub token encryption key"))?;
    let plaintext = cipher
        .decrypt(Nonce::from_slice(&nonce), ciphertext.as_ref())
        .map_err(|_| anyhow!("failed to decrypt GitHub access token"))?;

    String::from_utf8(plaintext).context("GitHub access token is not valid UTF-8")
}

fn encryption_key(encryption_secret: &str) -> [u8; 32] {
    Sha256::digest(encryption_secret.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_access_token_encryption_round_trips_without_plaintext() {
        let encrypted = encrypt_access_token("github-token", "secret").unwrap();

        assert!(!encrypted.contains("github-token"));
        assert_eq!(
            decrypt_access_token(&encrypted, "secret").unwrap(),
            "github-token"
        );
        assert!(decrypt_access_token(&encrypted, "wrong-secret").is_err());
    }
}
