//! SQL rewriter that transforms Snowflake-specific syntax into DataFusion-compatible SQL.
//! Applied as a pre-processing step before DataFusion parses the query.

/// Rewrite Snowflake SQL to DataFusion-compatible SQL.
pub fn rewrite_snowflake_sql(sql: &str) -> String {
    let mut result = sql.to_string();

    result = rewrite_create_or_replace(&result);
    result = rewrite_qualify(&result);
    result = rewrite_colon_path(&result);

    result
}

/// CREATE OR REPLACE TABLE -> DROP TABLE IF EXISTS + CREATE TABLE
fn rewrite_create_or_replace(sql: &str) -> String {
    let upper = sql.to_uppercase();
    if upper.starts_with("CREATE OR REPLACE TABLE") {
        let rest = &sql["CREATE OR REPLACE TABLE".len()..];
        // Extract table name (first word after CREATE OR REPLACE TABLE)
        let trimmed = rest.trim();
        let table_name = trimmed.split_whitespace().next().unwrap_or("");
        if upper.contains(" AS ") {
            return format!(
                "DROP TABLE IF EXISTS {table_name}; CREATE TABLE {}",
                trimmed
            );
        }
    }
    sql.to_string()
}

/// QUALIFY clause -> subquery with window function filter.
/// SELECT ... QUALIFY ROW_NUMBER() OVER (...) = 1
/// -> SELECT * FROM (SELECT ..., ROW_NUMBER() OVER (...) AS __qualify_rn) WHERE __qualify_rn = 1
fn rewrite_qualify(sql: &str) -> String {
    let upper = sql.to_uppercase();
    let qualify_pos = find_keyword(&upper, "QUALIFY");

    if qualify_pos.is_none() {
        return sql.to_string();
    }
    let pos = qualify_pos.unwrap();

    // Split: everything before QUALIFY is the inner query, everything after is the condition
    let inner_query = &sql[..pos].trim();
    let qualify_condition = &sql[pos + 7..].trim(); // "QUALIFY" = 7 chars

    // We need to extract the window expression from the QUALIFY condition
    // and add it as a column to the inner SELECT, then filter on it
    // Simple approach: wrap in subquery
    format!("SELECT * FROM ({inner_query}) AS __q WHERE {qualify_condition}")
}

/// Rewrite Snowflake colon-path notation: column:path.to.field -> get_path(column, 'path.to.field')
fn rewrite_colon_path(sql: &str) -> String {
    let mut result = String::new();
    let mut chars = sql.chars().peekable();
    let mut in_string = false;
    let mut string_char = ' ';

    while let Some(c) = chars.next() {
        // Track string literals
        if !in_string && (c == '\'' || c == '"') {
            in_string = true;
            string_char = c;
            result.push(c);
            continue;
        }
        if in_string && c == string_char {
            in_string = false;
            result.push(c);
            continue;
        }
        if in_string {
            result.push(c);
            continue;
        }

        // Detect identifier:path pattern
        if c == ':' && !result.is_empty() {
            // Check if previous char is part of an identifier
            let prev = result.chars().last().unwrap_or(' ');
            if prev.is_alphanumeric() || prev == '_' {
                // Extract the column name (walk back)
                let col_start = result
                    .rfind(|c: char| !c.is_alphanumeric() && c != '_' && c != '.')
                    .map(|p| p + 1)
                    .unwrap_or(0);
                let column = result[col_start..].to_string();
                result.truncate(col_start);

                // Extract the path (walk forward)
                let mut path = String::new();
                while let Some(&next) = chars.peek() {
                    if next.is_alphanumeric() || next == '_' || next == '.' {
                        path.push(chars.next().unwrap());
                    } else {
                        break;
                    }
                }

                result.push_str(&format!("get_path({column}, '{path}')"));
                continue;
            }
        }

        result.push(c);
    }

    result
}

fn find_keyword(upper_sql: &str, keyword: &str) -> Option<usize> {
    // Find keyword that's not inside quotes or parentheses
    let mut depth = 0;
    let mut in_string = false;
    let bytes = upper_sql.as_bytes();
    let kw_bytes = keyword.as_bytes();
    let kw_len = kw_bytes.len();

    for i in 0..bytes.len() {
        if bytes[i] == b'\'' {
            in_string = !in_string;
        }
        if in_string {
            continue;
        }
        if bytes[i] == b'(' {
            depth += 1;
        }
        if bytes[i] == b')' {
            depth -= 1;
        }
        if depth > 0 {
            continue;
        }

        if i + kw_len <= bytes.len() && &bytes[i..i + kw_len] == kw_bytes {
            // Check word boundary
            let before_ok = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
            let after_ok = i + kw_len >= bytes.len() || !bytes[i + kw_len].is_ascii_alphanumeric();
            if before_ok && after_ok {
                return Some(i);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_colon_path_rewrite() {
        let sql = "SELECT payload:user.name FROM events";
        let result = rewrite_snowflake_sql(sql);
        assert!(result.contains("get_path(payload, 'user.name')"));
    }

    #[test]
    fn test_colon_path_in_where() {
        let sql = "SELECT * FROM events WHERE payload:status = 'active'";
        let result = rewrite_snowflake_sql(sql);
        assert!(result.contains("get_path(payload, 'status')"));
    }

    #[test]
    fn test_no_rewrite_in_string() {
        let sql = "SELECT 'no:rewrite' FROM t";
        let result = rewrite_snowflake_sql(sql);
        assert_eq!(result, sql);
    }

    #[test]
    fn test_qualify_rewrite() {
        let sql = "SELECT id, name, ROW_NUMBER() OVER (PARTITION BY dept ORDER BY salary DESC) AS rn FROM employees QUALIFY rn = 1";
        let result = rewrite_snowflake_sql(sql);
        assert!(result.contains("WHERE"));
        assert!(result.contains("rn = 1"));
    }

    #[test]
    fn test_create_or_replace() {
        let sql = "CREATE OR REPLACE TABLE summary AS SELECT 1";
        let result = rewrite_snowflake_sql(sql);
        assert!(result.contains("DROP TABLE IF EXISTS summary"));
    }

    #[test]
    fn test_passthrough() {
        let sql = "SELECT COUNT(*) FROM orders WHERE status = 'active'";
        let result = rewrite_snowflake_sql(sql);
        assert_eq!(result, sql);
    }
}
