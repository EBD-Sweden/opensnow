use anyhow::{Context, Result, bail};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use tracing::info;

/// Built-in roles with a defined hierarchy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BuiltinRole {
    AccountAdmin,
    SysAdmin,
    Public,
}

impl BuiltinRole {
    /// Numeric priority (higher = more privileged).
    pub fn priority(self) -> u32 {
        match self {
            Self::AccountAdmin => 100,
            Self::SysAdmin => 50,
            Self::Public => 0,
        }
    }

    /// Parse a role name string into a BuiltinRole, if it matches.
    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_uppercase().as_str() {
            "ACCOUNTADMIN" => Some(Self::AccountAdmin),
            "SYSADMIN" => Some(Self::SysAdmin),
            "PUBLIC" => Some(Self::Public),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::AccountAdmin => "ACCOUNTADMIN",
            Self::SysAdmin => "SYSADMIN",
            Self::Public => "PUBLIC",
        }
    }
}

/// Returns the priority of a role by name.
/// Built-in roles use their defined priority; custom roles get priority 10.
pub fn role_priority(role_name: &str) -> u32 {
    BuiltinRole::from_name(role_name)
        .map(|r| r.priority())
        .unwrap_or(10)
}

/// Returns true if `role_a` is equal to or higher in hierarchy than `role_b`.
pub fn role_includes(role_a: &str, role_b: &str) -> bool {
    role_priority(role_a) >= role_priority(role_b)
}

/// Role store backed by SQLite.
pub struct RoleStore {
    conn: Arc<Mutex<Connection>>,
}

impl RoleStore {
    pub fn new(conn: Arc<Mutex<Connection>>) -> Result<Self> {
        {
            let db = conn.lock().unwrap();
            db.execute_batch(
                "CREATE TABLE IF NOT EXISTS roles (
                    name TEXT PRIMARY KEY
                );
                CREATE TABLE IF NOT EXISTS user_roles (
                    username TEXT NOT NULL,
                    role_name TEXT NOT NULL,
                    PRIMARY KEY (username, role_name)
                );",
            )
            .context("failed to create role tables")?;

            // Seed built-in roles
            for builtin in &["ACCOUNTADMIN", "SYSADMIN", "PUBLIC"] {
                db.execute(
                    "INSERT OR IGNORE INTO roles (name) VALUES (?1)",
                    rusqlite::params![builtin],
                )?;
            }
        }
        Ok(Self { conn })
    }

    /// Create a custom role.
    pub fn create_role(&self, name: &str) -> Result<()> {
        let db = self.conn.lock().unwrap();
        db.execute(
            "INSERT INTO roles (name) VALUES (?1)",
            rusqlite::params![name],
        )
        .context("failed to create role (may already exist)")?;
        info!(role = name, "created role");
        Ok(())
    }

    /// Grant a role to a user.
    pub fn grant_role(&self, role: &str, to_user: &str) -> Result<()> {
        let db = self.conn.lock().unwrap();
        // Verify role exists
        let exists: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM roles WHERE name = ?1)",
            rusqlite::params![role],
            |row| row.get(0),
        )?;
        if !exists {
            bail!("role '{}' does not exist", role);
        }
        db.execute(
            "INSERT OR IGNORE INTO user_roles (username, role_name) VALUES (?1, ?2)",
            rusqlite::params![to_user, role],
        )?;
        info!(role, user = to_user, "granted role");
        Ok(())
    }

    /// Revoke a role from a user.
    pub fn revoke_role(&self, role: &str, from_user: &str) -> Result<()> {
        let db = self.conn.lock().unwrap();
        let deleted = db.execute(
            "DELETE FROM user_roles WHERE username = ?1 AND role_name = ?2",
            rusqlite::params![from_user, role],
        )?;
        if deleted == 0 {
            bail!("user '{}' does not have role '{}'", from_user, role);
        }
        info!(role, user = from_user, "revoked role");
        Ok(())
    }

    /// List all roles granted to a user.
    pub fn user_roles(&self, username: &str) -> Result<Vec<String>> {
        let db = self.conn.lock().unwrap();
        let mut stmt = db.prepare("SELECT role_name FROM user_roles WHERE username = ?1")?;
        let roles = stmt
            .query_map(rusqlite::params![username], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(roles)
    }

    /// List all defined roles.
    pub fn list_roles(&self) -> Result<Vec<String>> {
        let db = self.conn.lock().unwrap();
        let mut stmt = db.prepare("SELECT name FROM roles ORDER BY name")?;
        let roles = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(roles)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn test_conn() -> Arc<Mutex<Connection>> {
        Arc::new(Mutex::new(Connection::open_in_memory().unwrap()))
    }

    #[test]
    fn test_builtin_roles_seeded() {
        let conn = test_conn();
        let store = RoleStore::new(conn).unwrap();
        let roles = store.list_roles().unwrap();
        assert!(roles.contains(&"ACCOUNTADMIN".to_string()));
        assert!(roles.contains(&"SYSADMIN".to_string()));
        assert!(roles.contains(&"PUBLIC".to_string()));
    }

    #[test]
    fn test_create_custom_role() {
        let conn = test_conn();
        let store = RoleStore::new(conn).unwrap();
        store.create_role("ANALYST").unwrap();
        let roles = store.list_roles().unwrap();
        assert!(roles.contains(&"ANALYST".to_string()));
    }

    #[test]
    fn test_grant_and_revoke() {
        let conn = test_conn();
        let store = RoleStore::new(conn).unwrap();
        store.grant_role("SYSADMIN", "alice").unwrap();
        let roles = store.user_roles("alice").unwrap();
        assert_eq!(roles, vec!["SYSADMIN"]);

        store.revoke_role("SYSADMIN", "alice").unwrap();
        let roles = store.user_roles("alice").unwrap();
        assert!(roles.is_empty());
    }

    #[test]
    fn test_role_hierarchy() {
        assert!(role_includes("ACCOUNTADMIN", "SYSADMIN"));
        assert!(role_includes("SYSADMIN", "PUBLIC"));
        assert!(role_includes("ACCOUNTADMIN", "PUBLIC"));
        assert!(!role_includes("PUBLIC", "SYSADMIN"));
        assert!(!role_includes("SYSADMIN", "ACCOUNTADMIN"));
        // Custom role is above PUBLIC but below SYSADMIN
        assert!(role_includes("CUSTOM", "PUBLIC"));
        assert!(!role_includes("CUSTOM", "SYSADMIN"));
    }
}
