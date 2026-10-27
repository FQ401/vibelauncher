//! The account list on disk: public profile data only, never tokens.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use vl_core::AccountId;
use vl_core::account::Account;

use crate::error::AuthError;

const FORMAT: u32 = 1;

#[derive(Serialize, Deserialize)]
struct File {
    version: u32,
    accounts: Vec<Account>,
}

/// The accounts the launcher knows, saved atomically to one JSON file.
#[derive(Debug)]
pub struct AccountBook {
    path: PathBuf,
    accounts: Vec<Account>,
}

impl AccountBook {
    /// Loads the book at `path`. A missing file is an empty book; a damaged or newer file is set
    /// aside as `<name>.damaged` and the book starts empty, so the launcher still starts.
    pub fn load(path: &Path) -> Result<Self, AuthError> {
        let accounts = match fs::read(path) {
            Ok(bytes) => match serde_json::from_slice::<File>(&bytes) {
                Ok(file) if file.version <= FORMAT => file.accounts,
                _ => {
                    let mut aside = path.as_os_str().to_owned();
                    aside.push(".damaged");
                    let _ = fs::rename(path, aside);
                    Vec::new()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(AuthError::Store(format!("read account list: {}", e.kind()))),
        };
        Ok(Self {
            path: path.to_owned(),
            accounts,
        })
    }

    /// The accounts in list order.
    pub fn accounts(&self) -> &[Account] {
        &self.accounts
    }

    /// The account with `id`.
    pub fn get(&self, id: &AccountId) -> Option<&Account> {
        self.accounts.iter().find(|a| &a.id == id)
    }

    /// Adds `account`, or replaces the account with the same id (keeping its default flag). The
    /// first account becomes the default.
    pub fn upsert(&mut self, mut account: Account) {
        match self.accounts.iter_mut().find(|a| a.id == account.id) {
            Some(existing) => {
                account.is_default = existing.is_default;
                *existing = account;
            }
            None => {
                account.is_default = self.accounts.is_empty();
                self.accounts.push(account);
            }
        }
    }

    /// Removes an account; when it was the default, the first remaining one becomes the default.
    pub fn remove(&mut self, id: &AccountId) -> Option<Account> {
        let index = self.accounts.iter().position(|a| &a.id == id)?;
        let removed = self.accounts.remove(index);
        if removed.is_default
            && let Some(first) = self.accounts.first_mut()
        {
            first.is_default = true;
        }
        Some(removed)
    }

    /// Makes `id` the only default; `false` when there is no such account.
    pub fn set_default(&mut self, id: &AccountId) -> bool {
        if self.get(id).is_none() {
            return false;
        }
        for a in &mut self.accounts {
            a.is_default = &a.id == id;
        }
        true
    }

    /// Marks whether `id` must sign in again; `false` when there is no such account.
    pub fn set_needs_sign_in(&mut self, id: &AccountId, needs: bool) -> bool {
        match self.accounts.iter_mut().find(|a| &a.id == id) {
            Some(a) => {
                a.needs_sign_in = needs;
                true
            }
            None => false,
        }
    }

    /// Writes the book to disk atomically.
    pub fn save(&self) -> Result<(), AuthError> {
        let bytes = serde_json::to_vec_pretty(&File {
            version: FORMAT,
            accounts: self.accounts.clone(),
        })
        .map_err(|e| AuthError::Store(format!("encode account list: {e}")))?;
        vl_store::write_atomic(&self.path, &bytes).map_err(|e| AuthError::Store(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use vl_core::account::AccountKind;

    use super::*;

    fn account(n: u8) -> Account {
        Account {
            id: AccountId::new(&format!("id-{n}")).unwrap(),
            kind: AccountKind::Microsoft,
            name: format!("Player{n}"),
            uuid: format!("00000000-0000-4000-8000-{n:012}"),
            is_default: false,
            needs_sign_in: false,
        }
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vl-auth-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir.join("accounts.json")
    }

    #[test]
    fn defaults_follow_the_list() {
        let mut book = AccountBook::load(&temp("defaults")).unwrap();
        book.upsert(account(1));
        book.upsert(account(2));
        assert!(book.accounts()[0].is_default && !book.accounts()[1].is_default);
        // Replacing keeps the default flag.
        let mut again = account(1);
        again.name = "Renamed".into();
        book.upsert(again);
        assert!(book.accounts()[0].is_default);
        assert_eq!(book.accounts()[0].name, "Renamed");

        assert!(book.set_default(&account(2).id));
        assert!(!book.set_default(&AccountId::new("nope").unwrap()));
        book.remove(&account(2).id).unwrap();
        assert!(book.accounts()[0].is_default);
        assert!(book.remove(&account(2).id).is_none());
    }

    #[test]
    fn saves_and_reloads() {
        let path = temp("reload");
        let mut book = AccountBook::load(&path).unwrap();
        book.upsert(account(1));
        book.set_needs_sign_in(&account(1).id, true);
        book.save().unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.to_lowercase().contains("token"));
        let loaded = AccountBook::load(&path).unwrap();
        assert_eq!(loaded.accounts().len(), 1);
        assert!(loaded.accounts()[0].needs_sign_in);
    }

    #[test]
    fn a_damaged_file_is_set_aside() {
        let path = temp("damaged");
        fs::write(&path, b"{ not json").unwrap();
        let book = AccountBook::load(&path).unwrap();
        assert!(book.accounts().is_empty());
        assert!(path.with_file_name("accounts.json.damaged").exists());
    }
}
