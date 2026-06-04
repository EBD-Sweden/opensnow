use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use opensnow_auth::{ObjectType, Privilege, PrivilegeStore};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::auth::AuthContext;

#[derive(Clone)]
pub struct ObjectPolicyStore {
    privileges: Arc<PrivilegeStore>,
}

impl std::fmt::Debug for ObjectPolicyStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ObjectPolicyStore").finish_non_exhaustive()
    }
}

impl ObjectPolicyStore {
    pub fn new(privileges: PrivilegeStore) -> Self {
        Self {
            privileges: Arc::new(privileges),
        }
    }

    pub fn in_memory() -> anyhow::Result<Self> {
        let conn = Arc::new(Mutex::new(Connection::open_in_memory()?));
        Ok(Self::new(PrivilegeStore::new(conn)?))
    }

    pub fn from_catalog_path(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let conn = Arc::new(Mutex::new(Connection::open(path)?));
        Ok(Self::new(PrivilegeStore::new(conn)?))
    }

    pub fn grant_table_privilege(
        &self,
        role: &str,
        privilege: Privilege,
        table: &str,
    ) -> anyhow::Result<()> {
        self.privileges
            .grant_privilege(role, privilege, ObjectType::Table, table)
    }

    pub fn check_sql(&self, auth: &AuthContext, sql: &str) -> PolicyDecision {
        if auth.is_platform_admin() {
            return PolicyDecision::Allow;
        }

        let requirements = analyze_sql(sql);
        if requirements.is_empty() && !is_safe_objectless_statement(sql) {
            warn!(
                user = %auth.username,
                role = %auth.role,
                tenant = %auth.tenant_id,
                "object policy denied unsupported or unparseable SQL"
            );
            return PolicyDecision::Deny(PolicyDenial {
                privilege: Privilege::All,
                object_type: ObjectType::Database,
                object_name: "<unsupported-sql>".to_string(),
            });
        }
        for req in &requirements {
            match self.privileges.check_privilege(
                &auth.role,
                req.privilege,
                req.object_type,
                &req.object_name,
            ) {
                Ok(true) => {
                    info!(
                        user = %auth.username,
                        role = %auth.role,
                        tenant = %auth.tenant_id,
                        privilege = %req.privilege,
                        object_type = %req.object_type,
                        object_name = %req.object_name,
                        "object policy allowed SQL requirement"
                    );
                }
                Ok(false) => {
                    warn!(
                        user = %auth.username,
                        role = %auth.role,
                        tenant = %auth.tenant_id,
                        privilege = %req.privilege,
                        object_type = %req.object_type,
                        object_name = %req.object_name,
                        "object policy denied SQL requirement"
                    );
                    return PolicyDecision::Deny(PolicyDenial {
                        privilege: req.privilege,
                        object_type: req.object_type,
                        object_name: req.object_name.clone(),
                    });
                }
                Err(e) => {
                    warn!(
                        user = %auth.username,
                        role = %auth.role,
                        tenant = %auth.tenant_id,
                        error = %e,
                        "object policy check failed closed"
                    );
                    return PolicyDecision::Deny(PolicyDenial {
                        privilege: req.privilege,
                        object_type: req.object_type,
                        object_name: req.object_name.clone(),
                    });
                }
            }
        }
        PolicyDecision::Allow
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PolicyDecision {
    Allow,
    Deny(PolicyDenial),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyDenial {
    pub privilege: Privilege,
    pub object_type: ObjectType,
    pub object_name: String,
}

impl PolicyDenial {
    pub fn message(&self) -> String {
        format!(
            "object policy denied: role lacks {} on {} {}",
            self.privilege, self.object_type, self.object_name
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlRequirement {
    pub privilege: Privilege,
    pub object_type: ObjectType,
    pub object_name: String,
}

pub fn analyze_sql(sql: &str) -> Vec<SqlRequirement> {
    let tokens = tokenize_sql(sql);
    if tokens.is_empty() {
        return Vec::new();
    }

    let mut reqs = Vec::new();
    let first = tokens[0].to_ascii_uppercase();
    match first.as_str() {
        "SELECT" | "WITH" => collect_select_requirements(&tokens, &mut reqs),
        "DROP" => collect_typed_object_requirement(&tokens, &mut reqs, 1, Privilege::Drop),
        "INSERT" => {
            if let Some(into_idx) = find_token(&tokens, "INTO") {
                if let Some(name) = next_object_after(&tokens, into_idx + 1) {
                    push_table_req(&mut reqs, Privilege::Insert, name);
                }
            }
            collect_select_requirements(&tokens, &mut reqs);
        }
        "CREATE" => {
            collect_typed_object_requirement(&tokens, &mut reqs, 1, Privilege::Create);
            collect_select_requirements(&tokens, &mut reqs);
        }
        "ALTER" => collect_typed_object_requirement(&tokens, &mut reqs, 1, Privilege::Alter),
        _ => {}
    }
    reqs
}

fn collect_select_requirements(tokens: &[String], reqs: &mut Vec<SqlRequirement>) {
    for (idx, token) in tokens.iter().enumerate() {
        if matches!(token.to_ascii_uppercase().as_str(), "FROM" | "JOIN") {
            if let Some(name) = next_object_after(tokens, idx + 1) {
                if !name.eq_ignore_ascii_case("SELECT") && !name.eq_ignore_ascii_case("VALUES") {
                    push_table_req(reqs, Privilege::Select, name);
                }
            }
        }
    }
}

fn is_safe_objectless_statement(sql: &str) -> bool {
    let tokens = tokenize_sql(sql);
    tokens
        .first()
        .is_some_and(|first| matches!(first.to_ascii_uppercase().as_str(), "SELECT" | "WITH"))
}

fn collect_typed_object_requirement(
    tokens: &[String],
    reqs: &mut Vec<SqlRequirement>,
    kind_idx: usize,
    privilege: Privilege,
) {
    let Some(kind) = tokens.get(kind_idx) else {
        return;
    };
    let Some(object_type) = object_type_from_sql_keyword(kind) else {
        return;
    };
    if let Some(name) = next_object_after(tokens, kind_idx + 1) {
        push_object_req(reqs, privilege, object_type, name);
    }
}

fn object_type_from_sql_keyword(keyword: &str) -> Option<ObjectType> {
    match keyword.to_ascii_uppercase().as_str() {
        "DATABASE" => Some(ObjectType::Database),
        "SCHEMA" => Some(ObjectType::Schema),
        "WAREHOUSE" => Some(ObjectType::Warehouse),
        "STAGE" => Some(ObjectType::Stage),
        "TABLE" => Some(ObjectType::Table),
        _ => None,
    }
}

fn next_object_after(tokens: &[String], mut idx: usize) -> Option<&str> {
    while let Some(token) = tokens.get(idx) {
        let upper = token.to_ascii_uppercase();
        if matches!(upper.as_str(), "IF" | "NOT" | "EXISTS" | "ONLY") {
            idx += 1;
            continue;
        }
        if is_identifier_token(token) {
            return Some(token);
        }
        idx += 1;
    }
    None
}

fn find_token(tokens: &[String], needle: &str) -> Option<usize> {
    tokens.iter().position(|t| t.eq_ignore_ascii_case(needle))
}

fn push_table_req(reqs: &mut Vec<SqlRequirement>, privilege: Privilege, raw_name: &str) {
    push_object_req(reqs, privilege, ObjectType::Table, raw_name);
}

fn push_object_req(
    reqs: &mut Vec<SqlRequirement>,
    privilege: Privilege,
    object_type: ObjectType,
    raw_name: &str,
) {
    let object_name = normalize_object_name(raw_name);
    if object_name.is_empty()
        || reqs.iter().any(|r| {
            r.privilege == privilege && r.object_type == object_type && r.object_name == object_name
        })
    {
        return;
    }
    reqs.push(SqlRequirement {
        privilege,
        object_type,
        object_name,
    });
}

fn normalize_object_name(raw: &str) -> String {
    raw.trim_matches('"')
        .split('.')
        .next_back()
        .unwrap_or(raw)
        .trim_matches('"')
        .to_string()
}

fn is_identifier_token(token: &str) -> bool {
    token
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '"')
}

fn tokenize_sql(sql: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;

    for ch in sql.chars() {
        match ch {
            '\'' if !in_double => {
                in_single = !in_single;
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            '"' if !in_single => {
                in_double = !in_double;
                current.push(ch);
            }
            c if !in_single
                && !in_double
                && (c.is_whitespace() || matches!(c, ',' | ';' | '(' | ')')) =>
            {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            c if !in_single => current.push(c),
            _ => {}
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_auth(role: &str, scopes: Vec<&str>) -> AuthContext {
        AuthContext {
            user_id: 1,
            username: "test-user".to_string(),
            role: role.to_string(),
            tenant_id: "default".to_string(),
            scopes: scopes.into_iter().map(str::to_string).collect(),
        }
    }

    #[test]
    fn platform_admin_bypasses_object_privilege_checks() {
        let policy = ObjectPolicyStore::in_memory().unwrap();
        let auth = test_auth("SYSADMIN", vec![]);

        assert_eq!(
            policy.check_sql(&auth, "SELECT * FROM restricted_orders"),
            PolicyDecision::Allow
        );
    }

    #[test]
    fn analyzer_extracts_select_drop_insert_and_ctas_table_requirements() {
        let select = analyze_sql("SELECT * FROM public.allowed_orders JOIN dim_customer c ON true");
        assert!(
            select
                .iter()
                .any(|r| r.privilege == Privilege::Select && r.object_name == "allowed_orders")
        );
        assert!(
            select
                .iter()
                .any(|r| r.privilege == Privilege::Select && r.object_name == "dim_customer")
        );

        let drop = analyze_sql("DROP TABLE IF EXISTS secret_orders");
        assert_eq!(drop[0].privilege, Privilege::Drop);
        assert_eq!(drop[0].object_name, "secret_orders");

        let insert = analyze_sql("INSERT INTO facts SELECT * FROM staging_facts");
        assert!(
            insert
                .iter()
                .any(|r| r.privilege == Privilege::Insert && r.object_name == "facts")
        );
        assert!(
            insert
                .iter()
                .any(|r| r.privilege == Privilege::Select && r.object_name == "staging_facts")
        );

        let ctas = analyze_sql("CREATE TABLE report AS SELECT * FROM facts");
        assert!(
            ctas.iter()
                .any(|r| r.privilege == Privilege::Create && r.object_name == "report")
        );
        assert!(
            ctas.iter()
                .any(|r| r.privilege == Privilege::Select && r.object_name == "facts")
        );
    }

    #[test]
    fn analyzer_fails_closed_for_unsupported_or_unparseable_sql() {
        let policy = ObjectPolicyStore::in_memory().unwrap();
        let auth = test_auth("ANALYST", vec!["sql.query"]);

        let denied = policy.check_sql(&auth, "SHOW TABLES");
        assert!(matches!(denied, PolicyDecision::Deny(_)));

        let denied = policy.check_sql(&auth, "ALTER WAREHOUSE wh_x SET WAREHOUSE_SIZE = 'SMALL'");
        assert!(matches!(denied, PolicyDecision::Deny(_)));
    }

    #[test]
    fn durable_object_policy_store_reads_catalog_privileges() {
        let dir = tempfile::tempdir().unwrap();
        let catalog_path = dir.path().join("catalog.db");

        let durable = ObjectPolicyStore::from_catalog_path(&catalog_path).unwrap();
        durable
            .grant_table_privilege("ANALYST", Privilege::Select, "durable_orders")
            .unwrap();
        drop(durable);

        let reloaded = ObjectPolicyStore::from_catalog_path(&catalog_path).unwrap();
        let auth = test_auth("ANALYST", vec!["sql.query", "table.select"]);
        assert_eq!(
            reloaded.check_sql(&auth, "SELECT * FROM durable_orders"),
            PolicyDecision::Allow
        );
    }
}
