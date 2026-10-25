# CGDrive

**Satu halaman untuk upload file ke semua akun Google Drive kamu.**

Punya beberapa akun Google Drive? Daripada membuka akun satu per satu, hubungkan semuanya ke CGDrive, lalu cukup tarik & lepas file. CGDrive bisa otomatis memilih akun yang sisa ruangnya paling banyak, atau kamu pilih sendiri akun tujuannya.

Dibuat dengan **Rust** ([Axum](https://github.com/tokio-rs/axum) + [reqwest](https://github.com/seanmonstar/reqwest)) dengan antarmuka HTML/JS satu file tanpa framework.

## Fitur

- **Banyak akun** — hubungkan akun Google Drive sebanyak yang kamu mau lewat OAuth resmi Google.
- **Upload file apa saja**, banyak sekaligus, lewat drag & drop atau tombol *Pilih file*.
- **Pilih akun otomatis** — file dikirim ke akun dengan sisa kapasitas terbesar (bisa dimatikan).
- **Kapasitas tiap akun** terlihat langsung: terpakai, total, dan persentase.
- **Streaming** — file diteruskan langsung ke Google Drive memakai *resumable upload*, tidak ditampung di memori server, jadi file besar tetap aman.
- **Antrian dengan progres**, kecepatan, perkiraan waktu, tombol coba lagi, dan tautan *Buka di Drive*.
- Semua file masuk ke folder **`/CGDrive`** di tiap akun (dibuat otomatis).
- **Izin minimal** — memakai scope `drive.file`, jadi CGDrive hanya bisa melihat file yang ia upload sendiri, bukan seluruh isi Drive kamu.
- Tampilan bersih, responsif (enak dipakai di HP), dan mendukung mode gelap.

## Cara kerja

```
Browser ──(upload stream)──▶ CGDrive (Rust, lokal) ──(resumable upload)──▶ Google Drive akun A / B / C
                                  │
                                  └── data/accounts.json  (refresh token tiap akun)
```

Saat akun dihubungkan, CGDrive menyimpan *refresh token* akun itu. Setiap upload, CGDrive membuat *access token* baru, mengecek kuota, memilih akun, lalu meneruskan isi file langsung ke Google Drive.

## Persiapan: buat OAuth Client di Google Cloud

Langkah ini cukup dilakukan sekali.

1. Buka [Google Cloud Console](https://console.cloud.google.com/) dan buat project baru (misalnya `cgdrive`).
2. **APIs & Services → Library** → cari **Google Drive API** → **Enable**.
3. **APIs & Services → OAuth consent screen** (Google Auth Platform):
   - User type: **External**.
   - Isi nama aplikasi & email.
   - Di bagian **Audience / Test users**, tambahkan **semua alamat Gmail** yang ingin kamu hubungkan.
     Selama aplikasi berstatus *Testing*, hanya akun yang terdaftar di sini yang bisa login.
4. **APIs & Services → Credentials → Create credentials → OAuth client ID**:
   - Application type: **Web application**.
   - Authorized redirect URI: `http://localhost:8080/oauth/callback`
5. Salin **Client ID** dan **Client secret**.

> **Catatan:** Untuk aplikasi berstatus *Testing*, Google bisa membuat refresh token kedaluwarsa setelah 7 hari. Jika akun tiba-tiba gagal dimuat, cukup klik **Hubungkan akun Google** lagi dengan akun yang sama. Untuk menghindarinya, ubah status aplikasi ke *In production* di halaman OAuth consent screen (untuk pemakaian pribadi tidak perlu verifikasi selama kamu menerima peringatan "unverified app").

> **Client ID & secret cukup dibuat sekali.** Itu identitas *aplikasi* CGDrive, bukan identitas akun Gmail. Satu pasang dipakai untuk semua akun: setiap akun cukup login lewat tombol **Hubungkan akun Google**. Satu-satunya hal per akun adalah menambahkan email-nya sebagai *Test user* (langkah 3).

## Menjalankan

Butuh [Rust](https://rustup.rs/) 1.85 atau lebih baru.

```bash
git clone https://github.com/haloabdur/cgdrive.git
cd cgdrive

cp .env.example .env
# isi GOOGLE_CLIENT_ID dan GOOGLE_CLIENT_SECRET di file .env

cargo run --release
```

Buka **http://localhost:8080**, klik **Hubungkan akun Google**, dan ulangi untuk setiap akun. Setelah itu tinggal tarik & lepas file.

Untuk **memutuskan akun**, klik tombol **✕** di samping akun lalu konfirmasi. Akun dilepas dari CGDrive (refresh token-nya dihapus dari `data/accounts.json`), sedangkan file yang sudah di-upload tetap ada di Google Drive. Kalau ingin mencabut izinnya sepenuhnya dari sisi Google, buka [myaccount.google.com/permissions](https://myaccount.google.com/permissions).

## Konfigurasi

Semua konfigurasi lewat variabel lingkungan (atau file `.env`):

| Variabel | Wajib | Default | Keterangan |
| --- | --- | --- | --- |
| `GOOGLE_CLIENT_ID` | ya | — | Client ID OAuth dari Google Cloud |
| `GOOGLE_CLIENT_SECRET` | ya | — | Client secret OAuth |
| `CGDRIVE_ADDR` | tidak | `127.0.0.1:8080` | Alamat & port server |
| `CGDRIVE_REDIRECT_URI` | tidak | `http://localhost:<port>/oauth/callback` | Harus sama persis dengan redirect URI di Google Cloud |
| `CGDRIVE_DATA` | tidak | `data/accounts.json` | Lokasi file penyimpanan akun |
| `CGDRIVE_FOLDER` | tidak | `CGDrive` | Nama folder tujuan di setiap Drive |

## API

Antarmuka web memakai API sederhana ini, yang juga bisa kamu pakai dari skrip:

| Method | Path | Keterangan |
| --- | --- | --- |
| `GET` | `/api/accounts` | Daftar akun beserta kuota (`limit`, `usage` dalam byte) |
| `DELETE` | `/api/accounts/{id}` | Putuskan akun (file di Drive tidak dihapus) |
| `POST` | `/api/upload?account=auto\|{id}` | Upload isi body sebagai file. Header: `X-File-Name` (URL-encoded), `Content-Type`, `Content-Length` |

Contoh upload dari terminal:

```bash
curl -X POST "http://localhost:8080/api/upload?account=auto" \
  -H "X-File-Name: laporan.pdf" \
  -H "Content-Type: application/pdf" \
  --data-binary @laporan.pdf
```

## Keamanan

- `data/accounts.json` berisi **refresh token** semua akunmu. Jangan dibagikan atau di-commit (sudah ada di `.gitignore`). Di Linux/macOS file ini otomatis diberi izin `600`.
- Secara default server hanya mendengarkan di `127.0.0.1`, jadi hanya bisa diakses dari komputermu sendiri. CGDrive **belum punya login**: jangan membukanya ke internet/jaringan publik tanpa pengaman tambahan (misalnya reverse proxy dengan autentikasi).
- Mencabut akses kapan saja: [myaccount.google.com/permissions](https://myaccount.google.com/permissions).

## Struktur proyek

```
cgdrive/
├── Cargo.toml
├── src/
│   ├── main.rs     # server, rute, OAuth, pemilihan akun, upload
│   ├── google.rs   # klien Google OAuth & Drive API v3
│   ├── store.rs    # penyimpanan akun (JSON)
│   └── error.rs    # tipe error → respons JSON
└── static/
    └── index.html  # antarmuka (ditanam ke binary saat kompilasi)
```

## Pengembangan

```bash
cargo fmt
cargo clippy
cargo test
```

## Lisensi

[MIT](LICENSE) © 2026 Abdur Rahman Trimanto
