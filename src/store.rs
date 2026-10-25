//! Penyimpanan akun yang terhubung (file JSON lokal).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    pub email: String,
    pub name: Option<String>,
    pub picture: Option<String>,
    pub refresh_token: String,
    /// ID folder tujuan upload di Drive akun ini (dibuat otomatis).
    #[serde(default)]
    pub folder_id: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct StoreFile {
    accounts: Vec<Account>,
}

pub struct Store {
    path: PathBuf,
    accounts: Vec<Account>,
}

impl Store {
    pub fn load(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        let accounts = match std::fs::read_to_string(&path) {
            Ok(text) => {
                serde_json::from_str::<StoreFile>(&text)
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?
                    .accounts
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e),
        };
        Ok(Self { path, accounts })
    }

    pub fn accounts(&self) -> &[Account] {
        &self.accounts
    }

    pub fn get(&self, id: &str) -> Option<&Account> {
        self.accounts.iter().find(|a| a.id == id)
    }

    /// Tambah akun baru, atau perbarui akun dengan email yang sama.
    pub fn upsert(&mut self, account: Account) -> std::io::Result<()> {
        match self.accounts.iter_mut().find(|a| a.email == account.email) {
            Some(existing) => {
                existing.name = account.name;
                existing.picture = account.picture;
                existing.refresh_token = account.refresh_token;
            }
            None => self.accounts.push(account),
        }
        self.save()
    }

    pub fn set_folder(&mut self, id: &str, folder_id: String) -> std::io::Result<()> {
        if let Some(a) = self.accounts.iter_mut().find(|a| a.id == id) {
            a.folder_id = Some(folder_id);
        }
        self.save()
    }

    pub fn remove(&mut self, id: &str) -> std::io::Result<bool> {
        let before = self.accounts.len();
        self.accounts.retain(|a| a.id != id);
        let removed = self.accounts.len() != before;
        if removed {
            self.save()?;
        }
        Ok(removed)
    }

    fn save(&self) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent()
            && !dir.as_os_str().is_empty()
        {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(&StoreFile {
            accounts: self.accounts.clone(),
        })?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, text)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Berisi refresh token: hanya pemilik yang boleh membaca.
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(tmp, &self.path)
    }
}
