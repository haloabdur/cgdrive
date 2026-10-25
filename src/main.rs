//! CGDrive — satu halaman untuk upload file ke banyak akun Google Drive.

mod error;
mod google;
mod store;

use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{delete, get, post},
};
use error::AppError;
use futures_util::future::join_all;
use google::{OAuthConfig, Quota};
use rand::{Rng, distributions::Alphanumeric};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use store::{Account, Store};

const INDEX_HTML: &str = include_str!("../static/index.html");
const STATE_TTL: Duration = Duration::from_secs(10 * 60);

struct AppState {
    oauth: OAuthConfig,
    http: reqwest::Client,
    folder_name: String,
    store: Mutex<Store>,
    /// `state` OAuth yang sedang menunggu callback (perlindungan CSRF).
    pending: Mutex<HashMap<String, Instant>>,
    /// Access token per akun beserta waktu kedaluwarsanya.
    tokens: Mutex<HashMap<String, (String, Instant)>>,
}

type Shared = Arc<AppState>;

#[tokio::main]
async fn main() {
    let _ = dotenvy::dotenv();

    let required = |key: &str| {
        std::env::var(key).unwrap_or_else(|_| {
            eprintln!("Variabel lingkungan {key} belum diisi. Lihat README.md bagian Konfigurasi.");
            std::process::exit(1);
        })
    };

    let addr: SocketAddr = std::env::var("CGDRIVE_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:8080".into())
        .parse()
        .expect("CGDRIVE_ADDR tidak valid, contoh: 127.0.0.1:8080");
    let redirect_uri = std::env::var("CGDRIVE_REDIRECT_URI")
        .unwrap_or_else(|_| format!("http://localhost:{}/oauth/callback", addr.port()));
    let data_path = std::env::var("CGDRIVE_DATA").unwrap_or_else(|_| "data/accounts.json".into());

    let state = Arc::new(AppState {
        oauth: OAuthConfig {
            client_id: required("GOOGLE_CLIENT_ID"),
            client_secret: required("GOOGLE_CLIENT_SECRET"),
            redirect_uri,
        },
        http: reqwest::Client::builder()
            .user_agent(concat!("cgdrive/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("gagal membuat HTTP client"),
        folder_name: std::env::var("CGDRIVE_FOLDER").unwrap_or_else(|_| "CGDrive".into()),
        store: Mutex::new(Store::load(&data_path).expect("gagal membaca file akun")),
        pending: Mutex::new(HashMap::new()),
        tokens: Mutex::new(HashMap::new()),
    });

    let app = Router::new()
        .route("/", get(index))
        .route("/auth/google", get(auth_start))
        .route("/oauth/callback", get(auth_callback))
        .route("/api/accounts", get(list_accounts))
        .route("/api/accounts/{id}", delete(remove_account))
        .route("/api/upload", post(upload))
        .layer(DefaultBodyLimit::disable())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("gagal membuka port");
    println!("CGDrive berjalan di http://{addr}");
    axum::serve(listener, app).await.expect("server berhenti");
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

// ---------- OAuth ----------

async fn auth_start(State(s): State<Shared>) -> Redirect {
    let token: String = rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(32)
        .map(char::from)
        .collect();
    {
        let mut pending = s.pending.lock().unwrap();
        pending.retain(|_, t| t.elapsed() < STATE_TTL);
        pending.insert(token.clone(), Instant::now());
    }
    Redirect::to(&google::auth_url(&s.oauth, &token))
}

#[derive(Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

async fn auth_callback(State(s): State<Shared>, Query(q): Query<CallbackQuery>) -> Redirect {
    match finish_auth(&s, q).await {
        Ok(email) => Redirect::to(&format!("/?connected={}", urlencoding::encode(&email))),
        Err(e) => Redirect::to(&format!("/?error={}", urlencoding::encode(&e.message))),
    }
}

async fn finish_auth(s: &AppState, q: CallbackQuery) -> Result<String, AppError> {
    if let Some(err) = q.error {
        return Err(AppError::bad_request(format!("Izin dibatalkan ({err})")));
    }
    let valid = q
        .state
        .and_then(|st| s.pending.lock().unwrap().remove(&st))
        .is_some_and(|t| t.elapsed() < STATE_TTL);
    if !valid {
        return Err(AppError::bad_request(
            "Sesi login kedaluwarsa, silakan coba lagi",
        ));
    }
    let code = q
        .code
        .ok_or_else(|| AppError::bad_request("Kode otorisasi tidak ada"))?;

    let tokens = google::exchange_code(&s.http, &s.oauth, &code).await?;
    let refresh_token = tokens.refresh_token.ok_or_else(|| {
        AppError::bad_request("Google tidak memberi refresh token. Cabut akses CGDrive di myaccount.google.com/permissions lalu coba lagi")
    })?;
    let info = google::userinfo(&s.http, &tokens.access_token).await?;

    let account = Account {
        id: info.sub.clone(),
        email: info.email.clone(),
        name: info.name,
        picture: info.picture,
        refresh_token,
        folder_id: None,
    };
    s.store.lock().unwrap().upsert(account)?;
    cache_token(s, &info.sub, tokens.access_token, tokens.expires_in);
    Ok(info.email)
}

fn cache_token(s: &AppState, id: &str, token: String, expires_in: u64) {
    // Perbarui satu menit sebelum benar-benar kedaluwarsa.
    let until = Instant::now() + Duration::from_secs(expires_in.saturating_sub(60));
    s.tokens
        .lock()
        .unwrap()
        .insert(id.to_string(), (token, until));
}

async fn access_token(s: &AppState, account: &Account) -> Result<String, AppError> {
    if let Some((tok, until)) = s.tokens.lock().unwrap().get(&account.id)
        && Instant::now() < *until
    {
        return Ok(tok.clone());
    }
    let fresh = google::refresh(&s.http, &s.oauth, &account.refresh_token).await?;
    cache_token(s, &account.id, fresh.access_token.clone(), fresh.expires_in);
    Ok(fresh.access_token)
}

// ---------- API akun ----------

#[derive(Serialize)]
struct AccountView {
    id: String,
    email: String,
    name: Option<String>,
    picture: Option<String>,
    /// Byte; `null` = tanpa batas.
    limit: Option<u64>,
    usage: Option<u64>,
    error: Option<String>,
}

async fn account_quota(s: &AppState, a: &Account) -> Result<Quota, AppError> {
    let token = access_token(s, a).await?;
    google::quota(&s.http, &token).await
}

async fn list_accounts(State(s): State<Shared>) -> Json<Vec<AccountView>> {
    let accounts = s.store.lock().unwrap().accounts().to_vec();
    let quotas = join_all(accounts.iter().map(|a| account_quota(&s, a))).await;
    let views = accounts
        .into_iter()
        .zip(quotas)
        .map(|(a, q)| {
            let (limit, usage, error) = match q {
                Ok(q) => (q.limit, Some(q.usage), None),
                Err(e) => (None, None, Some(e.message)),
            };
            AccountView {
                id: a.id,
                email: a.email,
                name: a.name,
                picture: a.picture,
                limit,
                usage,
                error,
            }
        })
        .collect();
    Json(views)
}

async fn remove_account(
    State(s): State<Shared>,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    if !s.store.lock().unwrap().remove(&id)? {
        return Err(AppError::not_found("Akun tidak ditemukan"));
    }
    s.tokens.lock().unwrap().remove(&id);
    Ok(StatusCode::NO_CONTENT)
}

// ---------- Upload ----------

#[derive(Deserialize)]
struct UploadQuery {
    /// ID akun, atau `auto` untuk memilih akun dengan sisa ruang terbesar.
    account: Option<String>,
}

#[derive(Serialize)]
struct UploadResult {
    file: google::DriveFile,
    account_id: String,
    email: String,
}

async fn upload(
    State(s): State<Shared>,
    Query(q): Query<UploadQuery>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, AppError> {
    let size: u64 = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok()?.parse().ok())
        .ok_or_else(|| {
            AppError::new(
                StatusCode::LENGTH_REQUIRED,
                "Header Content-Length wajib ada",
            )
        })?;
    let name = headers
        .get("x-file-name")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| urlencoding::decode(v).ok())
        .map(|n| clean_name(&n))
        .filter(|n| !n.is_empty())
        .ok_or_else(|| AppError::bad_request("Header X-File-Name wajib ada"))?;
    let mime = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty())
        .unwrap_or("application/octet-stream")
        .to_string();

    let account = pick_account(&s, q.account.as_deref().unwrap_or("auto"), size).await?;
    let token = access_token(&s, &account).await?;
    let folder = upload_folder(&s, &account, &token).await?;

    let stream = reqwest::Body::wrap_stream(body.into_data_stream());
    let file = google::upload(&s.http, &token, &folder, &name, &mime, size, stream).await?;
    println!(
        "[upload] {} -> {} ({} byte)",
        file.name, account.email, size
    );

    Ok(Json(UploadResult {
        file,
        account_id: account.id,
        email: account.email,
    })
    .into_response())
}

async fn pick_account(s: &AppState, wanted: &str, size: u64) -> Result<Account, AppError> {
    let accounts = s.store.lock().unwrap().accounts().to_vec();
    if accounts.is_empty() {
        return Err(AppError::bad_request(
            "Belum ada akun Google Drive yang terhubung",
        ));
    }

    if wanted != "auto" {
        let account = s
            .store
            .lock()
            .unwrap()
            .get(wanted)
            .cloned()
            .ok_or_else(|| AppError::not_found("Akun tidak ditemukan"))?;
        let quota = account_quota(s, &account).await?;
        if quota.free() < size {
            return Err(AppError::new(
                StatusCode::INSUFFICIENT_STORAGE,
                format!("Ruang {} tidak cukup", account.email),
            ));
        }
        return Ok(account);
    }

    let quotas = join_all(accounts.iter().map(|a| account_quota(s, a))).await;
    accounts
        .into_iter()
        .zip(quotas)
        .filter_map(|(a, q)| q.ok().map(|q| (a, q.free())))
        .filter(|(_, free)| *free >= size)
        .max_by_key(|(_, free)| *free)
        .map(|(a, _)| a)
        .ok_or_else(|| {
            AppError::new(
                StatusCode::INSUFFICIENT_STORAGE,
                "Tidak ada akun dengan ruang yang cukup",
            )
        })
}

async fn upload_folder(s: &AppState, account: &Account, token: &str) -> Result<String, AppError> {
    if let Some(id) = &account.folder_id
        && google::folder_exists(&s.http, token, id).await?
    {
        return Ok(id.clone());
    }
    let id = google::ensure_folder(&s.http, token, &s.folder_name).await?;
    s.store
        .lock()
        .unwrap()
        .set_folder(&account.id, id.clone())?;
    Ok(id)
}

/// Ambil nama file saja (tanpa path) dan buang karakter kontrol.
fn clean_name(raw: &str) -> String {
    raw.rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::clean_name;

    #[test]
    fn clean_name_strips_paths_and_control_chars() {
        assert_eq!(clean_name("foto.jpg"), "foto.jpg");
        assert_eq!(clean_name("C:\\Users\\a\\laporan.pdf"), "laporan.pdf");
        assert_eq!(clean_name("../../etc/passwd"), "passwd");
        assert_eq!(clean_name(" nama\u{0007}file.txt "), "namafile.txt");
        assert_eq!(clean_name("dokumen ñ 日本.docx"), "dokumen ñ 日本.docx");
    }
}
