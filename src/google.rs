//! Klien kecil untuk Google OAuth 2.0 dan Google Drive API v3.

use crate::error::AppError;
use reqwest::{Client, StatusCode, header};
use serde::Deserialize;

const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const USERINFO_URL: &str = "https://openidconnect.googleapis.com/v1/userinfo";
const DRIVE_URL: &str = "https://www.googleapis.com/drive/v3";
const UPLOAD_URL: &str = "https://www.googleapis.com/upload/drive/v3/files";

/// `drive.file` hanya memberi akses ke file yang dibuat aplikasi ini,
/// cukup untuk upload dan jauh lebih aman daripada akses penuh.
const SCOPES: &str = "openid email profile https://www.googleapis.com/auth/drive.file";

const FOLDER_MIME: &str = "application/vnd.google-apps.folder";

#[derive(Clone)]
pub struct OAuthConfig {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
}

#[derive(Debug, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub expires_in: u64,
    pub refresh_token: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UserInfo {
    pub sub: String,
    pub email: String,
    pub name: Option<String>,
    pub picture: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct Quota {
    /// `None` berarti tanpa batas (misalnya beberapa akun Workspace).
    pub limit: Option<u64>,
    pub usage: u64,
}

impl Quota {
    pub fn free(&self) -> u64 {
        self.limit
            .map_or(u64::MAX, |l| l.saturating_sub(self.usage))
    }
}

#[derive(Debug, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DriveFile {
    pub id: String,
    pub name: String,
    pub web_view_link: Option<String>,
}

pub fn auth_url(cfg: &OAuthConfig, state: &str) -> String {
    format!(
        "{AUTH_URL}?client_id={}&redirect_uri={}&response_type=code&scope={}&access_type=offline&prompt=consent%20select_account&include_granted_scopes=true&state={}",
        urlencoding::encode(&cfg.client_id),
        urlencoding::encode(&cfg.redirect_uri),
        urlencoding::encode(SCOPES),
        urlencoding::encode(state),
    )
}

pub async fn exchange_code(
    http: &Client,
    cfg: &OAuthConfig,
    code: &str,
) -> Result<TokenResponse, AppError> {
    let res = http
        .post(TOKEN_URL)
        .form(&[
            ("code", code),
            ("client_id", &cfg.client_id),
            ("client_secret", &cfg.client_secret),
            ("redirect_uri", &cfg.redirect_uri),
            ("grant_type", "authorization_code"),
        ])
        .send()
        .await?;
    json_or_error(res).await
}

pub async fn refresh(
    http: &Client,
    cfg: &OAuthConfig,
    refresh_token: &str,
) -> Result<TokenResponse, AppError> {
    let res = http
        .post(TOKEN_URL)
        .form(&[
            ("refresh_token", refresh_token),
            ("client_id", &cfg.client_id),
            ("client_secret", &cfg.client_secret),
            ("grant_type", "refresh_token"),
        ])
        .send()
        .await?;
    json_or_error(res).await
}

pub async fn userinfo(http: &Client, token: &str) -> Result<UserInfo, AppError> {
    let res = http.get(USERINFO_URL).bearer_auth(token).send().await?;
    json_or_error(res).await
}

pub async fn quota(http: &Client, token: &str) -> Result<Quota, AppError> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct About {
        storage_quota: StorageQuota,
    }
    #[derive(Deserialize)]
    struct StorageQuota {
        // Drive mengembalikan angka sebagai string.
        limit: Option<String>,
        usage: Option<String>,
    }

    let res = http
        .get(format!("{DRIVE_URL}/about?fields=storageQuota"))
        .bearer_auth(token)
        .send()
        .await?;
    let about: About = json_or_error(res).await?;
    let parse = |v: Option<String>| v.and_then(|s| s.parse::<u64>().ok());
    Ok(Quota {
        limit: parse(about.storage_quota.limit),
        usage: parse(about.storage_quota.usage).unwrap_or(0),
    })
}

/// Cari folder bernama `name` di root, buat jika belum ada.
pub async fn ensure_folder(http: &Client, token: &str, name: &str) -> Result<String, AppError> {
    #[derive(Deserialize)]
    struct FileList {
        files: Vec<DriveFile>,
    }

    let q = format!(
        "name = '{}' and mimeType = '{FOLDER_MIME}' and 'root' in parents and trashed = false",
        name.replace('\\', "\\\\").replace('\'', "\\'")
    );
    let res = http
        .get(format!("{DRIVE_URL}/files"))
        .query(&[
            ("q", q.as_str()),
            ("fields", "files(id,name)"),
            ("pageSize", "1"),
        ])
        .bearer_auth(token)
        .send()
        .await?;
    let list: FileList = json_or_error(res).await?;
    if let Some(f) = list.files.into_iter().next() {
        return Ok(f.id);
    }

    let res = http
        .post(format!("{DRIVE_URL}/files?fields=id,name"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "name": name, "mimeType": FOLDER_MIME }))
        .send()
        .await?;
    let created: DriveFile = json_or_error(res).await?;
    Ok(created.id)
}

/// Apakah folder masih ada (belum dihapus permanen / dibuang ke sampah).
pub async fn folder_exists(http: &Client, token: &str, id: &str) -> Result<bool, AppError> {
    #[derive(Deserialize)]
    struct Meta {
        trashed: bool,
    }
    let res = http
        .get(format!("{DRIVE_URL}/files/{id}?fields=trashed"))
        .bearer_auth(token)
        .send()
        .await?;
    if res.status() == StatusCode::NOT_FOUND {
        return Ok(false);
    }
    let meta: Meta = json_or_error(res).await?;
    Ok(!meta.trashed)
}

/// Upload file memakai resumable upload, isi file di-stream tanpa ditampung di memori.
pub async fn upload(
    http: &Client,
    token: &str,
    folder_id: &str,
    name: &str,
    mime: &str,
    size: u64,
    body: reqwest::Body,
) -> Result<DriveFile, AppError> {
    let init = http
        .post(format!(
            "{UPLOAD_URL}?uploadType=resumable&fields=id,name,webViewLink"
        ))
        .bearer_auth(token)
        .header("X-Upload-Content-Type", mime)
        .header("X-Upload-Content-Length", size)
        .json(&serde_json::json!({ "name": name, "parents": [folder_id] }))
        .send()
        .await?;
    if !init.status().is_success() {
        return Err(google_error(init).await);
    }
    let session = init
        .headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| AppError::bad_gateway("Google tidak mengembalikan URL sesi upload"))?
        .to_string();

    let res = http
        .put(session)
        .header(header::CONTENT_TYPE, mime)
        .header(header::CONTENT_LENGTH, size)
        .body(body)
        .send()
        .await?;
    json_or_error(res).await
}

async fn json_or_error<T: serde::de::DeserializeOwned>(
    res: reqwest::Response,
) -> Result<T, AppError> {
    if !res.status().is_success() {
        return Err(google_error(res).await);
    }
    Ok(res.json::<T>().await?)
}

async fn google_error(res: reqwest::Response) -> AppError {
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    let message = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| {
            v.pointer("/error/message")
                .or_else(|| v.get("error_description"))
                .or_else(|| v.get("error"))
                .and_then(|m| m.as_str().map(str::to_owned))
        })
        .unwrap_or(body);
    AppError::bad_gateway(format!("Google API {status}: {message}"))
}
