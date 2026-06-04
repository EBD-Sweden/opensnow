pub const MAX_DEMO_QUERY_BYTES: usize = 64 * 1024;
pub const SQL_COMPATIBILITY_DOC: &str = "docs/SQL_COMPATIBILITY.md";

#[derive(Debug, Clone, PartialEq, Eq)]
enum TokenKind {
    Word,
    Symbol(char),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Token<'a> {
    text: &'a str,
    kind: TokenKind,
}

fn is_word_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

fn lex_sql(sql: &str) -> Vec<Token<'_>> {
    let mut tokens = Vec::new();
    let mut idx = 0;

    while idx < sql.len() {
        let ch = sql[idx..].chars().next().expect("idx is on char boundary");
        let ch_len = ch.len_utf8();

        if ch.is_whitespace() {
            idx += ch_len;
            continue;
        }

        if sql[idx..].starts_with("--") {
            idx += 2;
            while idx < sql.len() {
                let next = sql[idx..].chars().next().expect("idx is on char boundary");
                idx += next.len_utf8();
                if next == '\n' {
                    break;
                }
            }
            continue;
        }

        if sql[idx..].starts_with("/*") {
            idx += 2;
            while idx < sql.len() {
                if sql[idx..].starts_with("*/") {
                    idx += 2;
                    break;
                }
                let next = sql[idx..].chars().next().expect("idx is on char boundary");
                idx += next.len_utf8();
            }
            continue;
        }

        if ch == '\'' || ch == '"' {
            let quote = ch;
            idx += ch_len;
            while idx < sql.len() {
                let next = sql[idx..].chars().next().expect("idx is on char boundary");
                idx += next.len_utf8();
                if next == quote {
                    if sql[idx..].starts_with(quote) {
                        idx += quote.len_utf8();
                    } else {
                        break;
                    }
                }
            }
            continue;
        }

        if is_word_char(ch) {
            let start = idx;
            idx += ch_len;
            while idx < sql.len() {
                let next = sql[idx..].chars().next().expect("idx is on char boundary");
                if !is_word_char(next) {
                    break;
                }
                idx += next.len_utf8();
            }
            tokens.push(Token {
                text: &sql[start..idx],
                kind: TokenKind::Word,
            });
            continue;
        }

        tokens.push(Token {
            text: &sql[idx..idx + ch_len],
            kind: TokenKind::Symbol(ch),
        });
        idx += ch_len;
    }

    tokens
}

fn split_sql_statements(sql: &str) -> Vec<String> {
    let mut statements = Vec::new();
    let mut current = String::new();
    let mut chars = sql.chars().peekable();
    let mut quote: Option<char> = None;
    let mut line_comment = false;
    let mut block_comment = false;

    while let Some(ch) = chars.next() {
        current.push(ch);

        if line_comment {
            if ch == '\n' {
                line_comment = false;
            }
            continue;
        }

        if block_comment {
            if ch == '*' && chars.peek() == Some(&'/') {
                current.push(chars.next().unwrap());
                block_comment = false;
            }
            continue;
        }

        if let Some(q) = quote {
            if ch == q {
                if chars.peek() == Some(&q) {
                    current.push(chars.next().unwrap());
                } else {
                    quote = None;
                }
            }
            continue;
        }

        match ch {
            '-' if chars.peek() == Some(&'-') => {
                current.push(chars.next().unwrap());
                line_comment = true;
            }
            '/' if chars.peek() == Some(&'*') => {
                current.push(chars.next().unwrap());
                block_comment = true;
            }
            '\'' | '"' => quote = Some(ch),
            ';' => {
                let trimmed = current.trim().trim_end_matches(';').trim();
                if !trimmed.is_empty() && !lex_sql(trimmed).is_empty() {
                    statements.push(trimmed.to_string());
                }
                current.clear();
            }
            _ => {}
        }
    }

    let trimmed = current.trim();
    if !trimmed.is_empty() && !lex_sql(trimmed).is_empty() {
        statements.push(trimmed.to_string());
    }
    statements
}

pub fn first_keyword(sql: &str) -> String {
    lex_sql(sql)
        .into_iter()
        .find(|token| matches!(token.kind, TokenKind::Word))
        .map(|token| token.text.to_ascii_uppercase())
        .unwrap_or_default()
}

fn token_eq(token: Option<&Token<'_>>, expected: &str) -> bool {
    token
        .filter(|token| matches!(token.kind, TokenKind::Word))
        .is_some_and(|token| token.text.eq_ignore_ascii_case(expected))
}

fn contains_keyword(sql: &str, expected: &str) -> bool {
    lex_sql(sql).into_iter().any(|token| {
        matches!(token.kind, TokenKind::Word) && token.text.eq_ignore_ascii_case(expected)
    })
}

fn starts_with_keyword_sequence(sql: &str, expected: &[&str]) -> bool {
    let words: Vec<_> = lex_sql(sql)
        .into_iter()
        .filter(|token| matches!(token.kind, TokenKind::Word))
        .collect();
    words.len() >= expected.len()
        && words
            .iter()
            .take(expected.len())
            .zip(expected.iter())
            .all(|(actual, expected)| actual.text.eq_ignore_ascii_case(expected))
}

fn is_valid_demo_relation_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
}

fn relation_name_from_token_start<'sql>(sql: &'sql str, token: &Token<'sql>) -> &'sql str {
    let start = token.text.as_ptr() as usize - sql.as_ptr() as usize;
    let rest = &sql[start..];
    let end = rest
        .char_indices()
        .find(|(_, ch)| ch.is_whitespace())
        .map(|(idx, _)| idx)
        .unwrap_or(rest.len());
    &rest[..end]
}

fn relation_token_after_create_prefix<'sql>(
    sql: &'sql str,
    tokens: &[Token<'sql>],
    prefix_len: usize,
) -> Option<&'sql str> {
    let target = tokens.get(prefix_len)?;
    if target.text.eq_ignore_ascii_case("IF")
        && token_eq(tokens.get(prefix_len + 1), "NOT")
        && token_eq(tokens.get(prefix_len + 2), "EXISTS")
    {
        return tokens
            .get(prefix_len + 3)
            .map(|token| relation_name_from_token_start(sql, token));
    }
    Some(relation_name_from_token_start(sql, target))
}

fn create_relation_target(sql: &str) -> Option<&str> {
    let tokens = lex_sql(sql);
    if token_eq(tokens.first(), "CREATE") && token_eq(tokens.get(1), "TABLE") {
        relation_token_after_create_prefix(sql, &tokens, 2)
    } else if token_eq(tokens.first(), "CREATE")
        && token_eq(tokens.get(1), "MATERIALIZED")
        && token_eq(tokens.get(2), "VIEW")
    {
        relation_token_after_create_prefix(sql, &tokens, 3)
    } else {
        None
    }
}

fn create_materialization_query(sql: &str) -> Option<&str> {
    let is_materializing_create = starts_with_keyword_sequence(sql, &["CREATE", "TABLE"])
        || starts_with_keyword_sequence(sql, &["CREATE", "MATERIALIZED", "VIEW"]);
    if !is_materializing_create {
        return None;
    }

    let tokens = lex_sql(sql);
    tokens
        .iter()
        .position(|token| {
            matches!(token.kind, TokenKind::Word) && token.text.eq_ignore_ascii_case("AS")
        })
        .map(|idx| {
            let as_token = &tokens[idx];
            let as_end =
                as_token.text.as_ptr() as usize - sql.as_ptr() as usize + as_token.text.len();
            sql[as_end..].trim_start()
        })
}

fn validate_demo_materialization_query(sql: &str) -> Result<(), String> {
    let Some(query_sql) = create_materialization_query(sql) else {
        return Ok(());
    };
    match first_keyword(query_sql).as_str() {
        "SELECT" | "WITH" => Ok(()),
        keyword => Err(format!(
            "Unsupported SQL statement for the external demo: CREATE materialization query must start with SELECT or WITH, got {keyword}. See {SQL_COMPATIBILITY_DOC}."
        )),
    }
}

fn is_supported_demo_statement(sql: &str) -> bool {
    let first = first_keyword(sql);
    match first.as_str() {
        "SELECT" | "WITH" | "EXPLAIN" | "SHOW" | "DESCRIBE" | "DESC" => true,
        "CREATE" => {
            (starts_with_keyword_sequence(sql, &["CREATE", "TABLE"]) && contains_keyword(sql, "AS"))
                || starts_with_keyword_sequence(sql, &["CREATE", "MATERIALIZED", "VIEW"])
                || starts_with_keyword_sequence(sql, &["CREATE", "WAREHOUSE"])
        }
        "REFRESH" => starts_with_keyword_sequence(sql, &["REFRESH", "MATERIALIZED", "VIEW"]),
        "ALTER" => starts_with_keyword_sequence(sql, &["ALTER", "WAREHOUSE"]),
        "USE" => starts_with_keyword_sequence(sql, &["USE", "WAREHOUSE"]),
        _ => false,
    }
}

pub fn validate_demo_sql(sql: &str) -> Result<String, String> {
    if sql.trim().is_empty() {
        return Err("SQL must not be empty. Try: SELECT 1 AS smoke".to_string());
    }
    if sql.len() > MAX_DEMO_QUERY_BYTES {
        return Err(format!(
            "SQL text is too large for the external demo ({} bytes max). Use a smaller statement or load data through documented ingest paths.",
            MAX_DEMO_QUERY_BYTES
        ));
    }

    let statements = split_sql_statements(sql);
    if statements.len() != 1 {
        return Err(format!(
            "OpenSnow demo accepts one SQL statement per request; received {}. Split multi-step workflows into separate requests. See {SQL_COMPATIBILITY_DOC}.",
            statements.len()
        ));
    }

    let statement = statements.into_iter().next().unwrap();
    if let Some(target) = create_relation_target(&statement)
        && !is_valid_demo_relation_name(target)
    {
        return Err(format!(
            "invalid demo table identifier: {target}. Use an unquoted table name with letters, numbers, and underscores only. See {SQL_COMPATIBILITY_DOC}."
        ));
    }
    validate_demo_materialization_query(&statement)?;

    if !is_supported_demo_statement(&statement) {
        let keyword = first_keyword(&statement);
        return Err(format!(
            "Unsupported SQL statement for the external demo: {keyword}. Supported demo statements include SELECT/WITH/EXPLAIN, SHOW, DESCRIBE, CREATE TABLE AS SELECT with safe identifiers, CREATE/REFRESH MATERIALIZED VIEW, and warehouse SHOW/CREATE/ALTER/USE. Use /api/v1/ingest for demo loads. See {SQL_COMPATIBILITY_DOC}."
        ));
    }

    Ok(statement)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validator_rejects_destructive_and_surprising_sql() {
        for sql in [
            "DROP TABLE smoke_rollup",
            "DROP MATERIALIZED VIEW mv_demo",
            "TRUNCATE TABLE victim",
            "DELETE FROM victim WHERE id = 1",
            "UPDATE victim SET id = 2",
            "INSERT INTO victim VALUES (1)",
            "BEGIN",
            "COMMIT",
            "ROLLBACK",
            "COPY INTO public_smoke FROM '/tmp/public_smoke.parquet'",
            "CREATE TABLE safe AS DROP TABLE victim",
            "CREATE TABLE safe AS DELETE FROM victim",
            "CREATE TABLE safe AS UPDATE victim SET id = 2",
            "CREATE TABLE safe AS INSERT INTO victim VALUES (1)",
            "CREATE TABLE safe AS TRUNCATE TABLE victim",
            "CREATE TABLE safe AS /* comment */ DROP TABLE victim",
            "CREATE MATERIALIZED VIEW safe_mv AS DROP TABLE victim",
            "CREATE TABLE ../escape AS SELECT 1 AS smoke",
            "CREATE TABLE IF NOT EXISTS ../escape AS SELECT 1 AS smoke",
            "CREATE TABLE public.bad AS SELECT 1 AS smoke",
            "SELECT 1; DROP TABLE victim",
        ] {
            assert!(
                validate_demo_sql(sql).is_err(),
                "demo validator should reject unsafe SQL: {sql}"
            );
        }
    }

    #[test]
    fn validator_allows_destructive_words_in_comments_and_strings() {
        for sql in [
            "-- DROP TABLE is only documentation\nSELECT 1 AS smoke",
            "/* DELETE FROM docs only */ SELECT 'DROP TABLE as string' AS note",
            "SELECT 'TRUNCATE TABLE text only' AS note",
            "SELECT \"UPDATE\" FROM (SELECT 1 AS \"UPDATE\")",
        ] {
            validate_demo_sql(sql).unwrap_or_else(|err| {
                panic!("demo validator should ignore destructive words in comments/strings: {sql}: {err}")
            });
        }
    }

    #[test]
    fn validator_accepts_documented_public_demo_queries() {
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("../../../demo/public-demo-manifest.json"))
                .expect("public demo manifest is valid JSON");
        for check in manifest["checks"].as_array().expect("checks array exists") {
            let sql = check["sql"].as_str().expect("check SQL is a string");
            validate_demo_sql(sql).unwrap_or_else(|err| {
                panic!("manifest query should pass demo guardrails: {sql}: {err}")
            });
        }

        for sql in [
            "SELECT call_type, COUNT(*) AS calls FROM cdrs GROUP BY call_type ORDER BY call_type",
            "SELECT region, COUNT(*) AS subscribers FROM subscribers GROUP BY region ORDER BY subscribers DESC",
            "SELECT call_type, COUNT(*) AS calls FROM cdrs GROUP BY call_type ORDER BY call_type;",
            "SELECT region, COUNT(*) AS rows, SUM(amount) AS amount FROM public_smoke GROUP BY region ORDER BY region;",
        ] {
            validate_demo_sql(sql).unwrap_or_else(|err| {
                panic!("documented public-test query should pass demo guardrails: {sql}: {err}")
            });
        }
    }

    #[test]
    fn validator_enforces_size_cap() {
        let oversized = format!("SELECT '{}' AS payload", "x".repeat(MAX_DEMO_QUERY_BYTES));
        assert!(validate_demo_sql(&oversized).is_err());
    }
}
