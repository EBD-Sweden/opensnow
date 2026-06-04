use anyhow::{Context, Result, bail};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use std::{fmt, str::FromStr};
use tracing::info;

use crate::roles::role_includes;

/// Object types that can have privileges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ObjectType {
    Database,
    Schema,
    Warehouse,
    Stage,
    Table,
}

impl FromStr for ObjectType {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_uppercase().as_str() {
            "DATABASE" => Ok(Self::Database),
            "SCHEMA" => Ok(Self::Schema),
            "WAREHOUSE" => Ok(Self::Warehouse),
            "STAGE" => Ok(Self::Stage),
            "TABLE" => Ok(Self::Table),
            _ => bail!("unknown object type: {s}"),
        }
    }
}

impl fmt::Display for ObjectType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Database => write!(f, "DATABASE"),
            Self::Schema => write!(f, "SCHEMA"),
            Self::Warehouse => write!(f, "WAREHOUSE"),
            Self::Stage => write!(f, "STAGE"),
            Self::Table => write!(f, "TABLE"),
        }
    }
}

/// Privileges that can be granted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Privilege {
    Select,
    Insert,
    Create,
    Drop,
    Alter,
    All,
}

impl FromStr for Privilege {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_uppercase().as_str() {
            "SELECT" => Ok(Self::Select),
            "INSERT" => Ok(Self::Insert),
            "CREATE" => Ok(Self::Create),
            "DROP" => Ok(Self::Drop),
            "ALTER" => Ok(Self::Alter),
            "ALL" => Ok(Self::All),
            _ => bail!("unknown privilege: {s}"),
        }
    }
}

impl Privilege {
    /// Returns true if `self` covers `other` (e.g. ALL covers everything).
    pub fn covers(self, other: Self) -> bool {
        self == Privilege::All || self == other
    }
}

impl fmt::Display for Privilege {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Select => write!(f, "SELECT"),
            Self::Insert => write!(f, "INSERT"),
            Self::Create => write!(f, "CREATE"),
            Self::Drop => write!(f, "DROP"),
            Self::Alter => write!(f, "ALTER"),
            Self::All => write!(f, "ALL"),
        }
    }
}

/// Privilege store backed by SQLite.
pub struct PrivilegeStore {
    conn: Arc<Mutex<Connection>>,
}

impl PrivilegeStore {
    pub fn new(conn: Arc<Mutex<Connection>>) -> Result<Self> {
        {
            let db = conn.lock().unwrap();
            db.execute_batch(
                "CREATE TABLE IF NOT EXISTS privileges (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    role_name TEXT NOT NULL,
                    privilege TEXT NOT NULL,
                    object_type TEXT NOT NULL,
                    object_name TEXT NOT NULL,
                    UNIQUE(role_name, privilege, object_type, object_name)
                );",
            )
            .context("failed to create privileges table")?;
        }
        Ok(Self { conn })
    }

    /// Grant a privilege on an object to a role.
    pub fn grant_privilege(
        &self,
        role: &str,
        privilege: Privilege,
        object_type: ObjectType,
        object_name: &str,
    ) -> Result<()> {
        let db = self.conn.lock().unwrap();
        db.execute(
            "INSERT OR IGNORE INTO privileges (role_name, privilege, object_type, object_name)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                role,
                privilege.to_string(),
                object_type.to_string(),
                object_name,
            ],
        )
        .context("failed to grant privilege")?;
        info!(
            role,
            privilege = %privilege,
            object_type = %object_type,
            object_name,
            "granted privilege"
        );
        Ok(())
    }

    /// Revoke a privilege on an object from a role.
    pub fn revoke_privilege(
        &self,
        role: &str,
        privilege: Privilege,
        object_type: ObjectType,
        object_name: &str,
    ) -> Result<()> {
        let db = self.conn.lock().unwrap();
        let deleted = db.execute(
            "DELETE FROM privileges
             WHERE role_name = ?1 AND privilege = ?2 AND object_type = ?3 AND object_name = ?4",
            rusqlite::params![
                role,
                privilege.to_string(),
                object_type.to_string(),
                object_name,
            ],
        )?;
        if deleted == 0 {
            bail!("privilege not found");
        }
        info!(
            role,
            privilege = %privilege,
            object_type = %object_type,
            object_name,
            "revoked privilege"
        );
        Ok(())
    }

    /// Check whether a user (by their primary role) has a given privilege on an object.
    ///
    /// ACCOUNTADMIN always has all privileges. The check also considers the ALL
    /// privilege as covering any specific privilege.
    pub fn check_privilege(
        &self,
        user_role: &str,
        privilege: Privilege,
        object_type: ObjectType,
        object_name: &str,
    ) -> Result<bool> {
        // ACCOUNTADMIN bypasses all checks
        if user_role.to_uppercase() == "ACCOUNTADMIN" {
            return Ok(true);
        }

        let db = self.conn.lock().unwrap();
        let mut stmt = db.prepare(
            "SELECT privilege, role_name FROM privileges
             WHERE object_type = ?1 AND object_name = ?2",
        )?;

        let rows = stmt.query_map(
            rusqlite::params![object_type.to_string(), object_name],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )?;

        for row in rows {
            let (priv_str, grant_role) = row?;
            // Check if the user's role is high enough
            if !role_includes(user_role, &grant_role) {
                continue;
            }
            if priv_str
                .parse::<Privilege>()
                .is_ok_and(|granted_priv| granted_priv.covers(privilege))
            {
                return Ok(true);
            }
        }

        Ok(false)
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
    fn object_type_and_privilege_parse_case_insensitively() {
        assert_eq!(
            "database".parse::<ObjectType>().unwrap(),
            ObjectType::Database
        );
        assert_eq!("TABLE".parse::<ObjectType>().unwrap(), ObjectType::Table);
        assert!("unknown".parse::<ObjectType>().is_err());

        assert_eq!("select".parse::<Privilege>().unwrap(), Privilege::Select);
        assert_eq!("ALL".parse::<Privilege>().unwrap(), Privilege::All);
        assert!("grant".parse::<Privilege>().is_err());
    }

    #[test]
    fn test_grant_and_check() {
        let conn = test_conn();
        let store = PrivilegeStore::new(conn).unwrap();

        store
            .grant_privilege("SYSADMIN", Privilege::Select, ObjectType::Table, "orders")
            .unwrap();

        // SYSADMIN should have SELECT
        assert!(
            store
                .check_privilege("SYSADMIN", Privilege::Select, ObjectType::Table, "orders")
                .unwrap()
        );

        // SYSADMIN should NOT have INSERT
        assert!(
            !store
                .check_privilege("SYSADMIN", Privilege::Insert, ObjectType::Table, "orders")
                .unwrap()
        );
    }

    #[test]
    fn test_all_covers_specific() {
        let conn = test_conn();
        let store = PrivilegeStore::new(conn).unwrap();

        store
            .grant_privilege("SYSADMIN", Privilege::All, ObjectType::Table, "users")
            .unwrap();

        assert!(
            store
                .check_privilege("SYSADMIN", Privilege::Select, ObjectType::Table, "users")
                .unwrap()
        );
        assert!(
            store
                .check_privilege("SYSADMIN", Privilege::Insert, ObjectType::Table, "users")
                .unwrap()
        );
        assert!(
            store
                .check_privilege("SYSADMIN", Privilege::Drop, ObjectType::Table, "users")
                .unwrap()
        );
    }

    #[test]
    fn test_accountadmin_bypasses() {
        let conn = test_conn();
        let store = PrivilegeStore::new(conn).unwrap();

        // No privileges granted at all, but ACCOUNTADMIN always passes
        assert!(
            store
                .check_privilege(
                    "ACCOUNTADMIN",
                    Privilege::Drop,
                    ObjectType::Database,
                    "prod"
                )
                .unwrap()
        );
    }

    #[test]
    fn test_revoke() {
        let conn = test_conn();
        let store = PrivilegeStore::new(conn).unwrap();

        store
            .grant_privilege("SYSADMIN", Privilege::Select, ObjectType::Table, "t1")
            .unwrap();
        assert!(
            store
                .check_privilege("SYSADMIN", Privilege::Select, ObjectType::Table, "t1")
                .unwrap()
        );

        store
            .revoke_privilege("SYSADMIN", Privilege::Select, ObjectType::Table, "t1")
            .unwrap();
        assert!(
            !store
                .check_privilege("SYSADMIN", Privilege::Select, ObjectType::Table, "t1")
                .unwrap()
        );
    }

    #[test]
    fn test_role_hierarchy_privilege_check() {
        let conn = test_conn();
        let store = PrivilegeStore::new(conn).unwrap();

        // Grant SELECT to PUBLIC
        store
            .grant_privilege("PUBLIC", Privilege::Select, ObjectType::Table, "public_t")
            .unwrap();

        // SYSADMIN (higher) should inherit PUBLIC's privileges
        assert!(
            store
                .check_privilege("SYSADMIN", Privilege::Select, ObjectType::Table, "public_t")
                .unwrap()
        );

        // PUBLIC should NOT inherit SYSADMIN grants
        store
            .grant_privilege("SYSADMIN", Privilege::Insert, ObjectType::Table, "admin_t")
            .unwrap();
        assert!(
            !store
                .check_privilege("PUBLIC", Privilege::Insert, ObjectType::Table, "admin_t")
                .unwrap()
        );
    }
}
