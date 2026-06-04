use anyhow::{Context, Result, bail};
use argon2::password_hash::SaltString;
use argon2::password_hash::rand_core::OsRng;
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use tracing::info;

/// A user record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: i64,
    pub username: String,
    pub role: String,
}

/// User store backed by SQLite.
pub struct UserStore {
    conn: Arc<Mutex<Connection>>,
}

impl UserStore {
    /// Open or create the user store, creating tables if needed.
    /// If no users exist, an admin user is auto-created with the given default password.
    pub fn new(conn: Arc<Mutex<Connection>>, default_admin_password: &str) -> Result<Self> {
        {
            let db = conn.lock().unwrap();
            db.execute_batch(
                "CREATE TABLE IF NOT EXISTS users (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    username TEXT NOT NULL UNIQUE,
                    password_hash TEXT NOT NULL,
                    role TEXT NOT NULL DEFAULT 'PUBLIC'
                );",
            )
            .context("failed to create users table")?;
        }

        let store = Self { conn };
        store.auto_create_admin(default_admin_password)?;
        Ok(store)
    }

    fn auto_create_admin(&self, default_password: &str) -> Result<()> {
        let db = self.conn.lock().unwrap();
        let count: i64 = db.query_row("SELECT COUNT(*) FROM users", [], |row| row.get(0))?;
        if count == 0 {
            let hash = hash_password(default_password)?;
            db.execute(
                "INSERT INTO users (username, password_hash, role) VALUES (?1, ?2, ?3)",
                rusqlite::params!["admin", &hash, "ACCOUNTADMIN"],
            )?;
            info!("auto-created default admin user");
        }
        Ok(())
    }

    /// Create a new user with the given username, password, and role.
    pub fn create_user(&self, username: &str, password: &str, role: &str) -> Result<User> {
        let hash = hash_password(password)?;
        let db = self.conn.lock().unwrap();
        db.execute(
            "INSERT INTO users (username, password_hash, role) VALUES (?1, ?2, ?3)",
            rusqlite::params![username, &hash, role],
        )
        .context("failed to create user")?;
        let id = db.last_insert_rowid();
        Ok(User {
            id,
            username: username.to_string(),
            role: role.to_string(),
        })
    }

    /// Authenticate a user by username and password.
    pub fn authenticate(&self, username: &str, password: &str) -> Result<User> {
        let db = self.conn.lock().unwrap();
        let (id, hash, role): (i64, String, String) = db
            .query_row(
                "SELECT id, password_hash, role FROM users WHERE username = ?1",
                rusqlite::params![username],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .context("user not found")?;

        verify_password(password, &hash)?;

        Ok(User {
            id,
            username: username.to_string(),
            role,
        })
    }

    /// Change a user's password.
    pub fn change_password(
        &self,
        username: &str,
        old_password: &str,
        new_password: &str,
    ) -> Result<()> {
        // Verify old password first
        let _user = self.authenticate(username, old_password)?;

        let hash = hash_password(new_password)?;
        let db = self.conn.lock().unwrap();
        let updated = db.execute(
            "UPDATE users SET password_hash = ?1 WHERE username = ?2",
            rusqlite::params![&hash, username],
        )?;
        if updated == 0 {
            bail!("user not found");
        }
        Ok(())
    }

    /// List all users (without password hashes).
    pub fn list_users(&self) -> Result<Vec<User>> {
        let db = self.conn.lock().unwrap();
        let mut stmt = db.prepare("SELECT id, username, role FROM users")?;
        let users = stmt
            .query_map([], |row| {
                Ok(User {
                    id: row.get(0)?,
                    username: row.get(1)?,
                    role: row.get(2)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(users)
    }

    /// Drop (delete) a user.
    pub fn drop_user(&self, username: &str) -> Result<()> {
        let db = self.conn.lock().unwrap();
        let deleted = db.execute(
            "DELETE FROM users WHERE username = ?1",
            rusqlite::params![username],
        )?;
        if deleted == 0 {
            bail!("user '{}' not found", username);
        }
        info!(username, "dropped user");
        Ok(())
    }
}

/// Hash a password using Argon2id.
fn hash_password(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    let hash = argon2
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("failed to hash password: {}", e))?;
    Ok(hash.to_string())
}

/// Verify a password against a stored hash.
fn verify_password(password: &str, hash_str: &str) -> Result<()> {
    let parsed =
        PasswordHash::new(hash_str).map_err(|e| anyhow::anyhow!("invalid password hash: {}", e))?;
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .map_err(|_| anyhow::anyhow!("invalid password"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn test_conn() -> Arc<Mutex<Connection>> {
        Arc::new(Mutex::new(Connection::open_in_memory().unwrap()))
    }

    #[test]
    fn test_auto_create_admin() {
        let conn = test_conn();
        let store = UserStore::new(conn, "admin123").unwrap();
        let users = store.list_users().unwrap();
        assert_eq!(users.len(), 1);
        assert_eq!(users[0].username, "admin");
        assert_eq!(users[0].role, "ACCOUNTADMIN");
    }

    #[test]
    fn test_create_and_authenticate() {
        let conn = test_conn();
        let store = UserStore::new(conn, "admin123").unwrap();
        store.create_user("alice", "secret", "SYSADMIN").unwrap();
        let user = store.authenticate("alice", "secret").unwrap();
        assert_eq!(user.username, "alice");
        assert_eq!(user.role, "SYSADMIN");
    }

    #[test]
    fn test_wrong_password() {
        let conn = test_conn();
        let store = UserStore::new(conn, "admin123").unwrap();
        store.create_user("bob", "correct", "PUBLIC").unwrap();
        let result = store.authenticate("bob", "wrong");
        assert!(result.is_err());
    }

    #[test]
    fn test_change_password() {
        let conn = test_conn();
        let store = UserStore::new(conn, "admin123").unwrap();
        store.create_user("carol", "old_pass", "PUBLIC").unwrap();
        store
            .change_password("carol", "old_pass", "new_pass")
            .unwrap();
        let user = store.authenticate("carol", "new_pass").unwrap();
        assert_eq!(user.username, "carol");
        // Old password should no longer work
        assert!(store.authenticate("carol", "old_pass").is_err());
    }

    #[test]
    fn test_drop_user() {
        let conn = test_conn();
        let store = UserStore::new(conn, "admin123").unwrap();
        store.create_user("dave", "pass", "PUBLIC").unwrap();
        assert_eq!(store.list_users().unwrap().len(), 2); // admin + dave
        store.drop_user("dave").unwrap();
        assert_eq!(store.list_users().unwrap().len(), 1);
    }

    #[test]
    fn test_password_hashing_produces_different_hashes() {
        let h1 = hash_password("same").unwrap();
        let h2 = hash_password("same").unwrap();
        // Different salts should produce different hashes
        assert_ne!(h1, h2);
        // Both should verify
        verify_password("same", &h1).unwrap();
        verify_password("same", &h2).unwrap();
    }
}
