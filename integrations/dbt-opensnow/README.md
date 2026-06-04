# dbt-opensnow

A [dbt](https://www.getdbt.com/) adapter for [OpenSnow](https://github.com/opensnow/opensnow).

OpenSnow exposes a PostgreSQL wire protocol on port 5433. This adapter uses `psycopg2` to communicate with OpenSnow over that protocol, inheriting most behaviour from `dbt-postgres` while overriding the pieces where OpenSnow's SQL dialect differs.

## Installation

```bash
pip install dbt-opensnow
```

Or install from source:

```bash
cd integrations/dbt-opensnow
pip install -e .
```

## Configuration

Add an OpenSnow target to your `~/.dbt/profiles.yml`:

```yaml
my_project:
  target: dev
  outputs:
    dev:
      type: opensnow
      host: localhost
      port: 5433
      user: opensnow
      password: "{{ env_var('OPENSNOW_PASSWORD', '') }}"
      dbname: opensnow
      schema: public
      threads: 4
```

### Connection parameters

| Parameter    | Description                          | Default     |
|-------------|--------------------------------------|-------------|
| `type`      | Must be `opensnow`                   | (required)  |
| `host`      | Hostname of the OpenSnow instance    | `localhost` |
| `port`      | PG wire protocol port                | `5433`      |
| `user`      | Database username                    | `opensnow`  |
| `password`  | Database password                    | `""`        |
| `dbname`    | Database name                        | `opensnow`  |
| `schema`    | Default schema                       | `public`    |
| `threads`   | Number of concurrent threads         | `4`         |
| `sslmode`   | SSL mode (`disable`, `require`, etc.)| (optional)  |

## Usage

Initialize a new dbt project:

```bash
dbt init my_telecom_project
```

Select `opensnow` when prompted for the adapter type.

Run your models:

```bash
dbt run
dbt test
```

## Sample project

A sample project is included under `sample_project/`. It demonstrates staging and mart models for telecom call-detail-record (CDR) data.

```bash
cd sample_project
dbt run --profiles-dir .
```

The OpenSnow server must be started with trusted-local pgwire plus trusted SQL
for dbt table materializations:

```bash
OPENSNOW_ENABLE_PGWIRE=1 OPENSNOW_TRUSTED_SQL=1 opensnow start --enable-pgwire
```

`OPENSNOW_TRUSTED_SQL=1` is an operator-only local/trusted deployment flag. It
lifts the public-demo SQL gate on the pgwire path so dbt can issue `CREATE TABLE
AS`, `DROP TABLE`, and session-control statements. Do not enable it on public
unauthenticated demos.

## Required project macros

OpenSnow's engine needs a **bare, unqualified** `CREATE TABLE AS` target and has
**no `ALTER TABLE ... RENAME`**, so the default dbt table materialization does
not work. dbt (>= 1.8) also does not apply an *imported package's*
materialization/dispatch overrides by default, so the OpenSnow-specific
materialization and `create_table_as`/`drop_relation` macros must live in **your
dbt project's own `macros/`** directory.

Copy `sample_project/macros/opensnow_project.sql` into every OpenSnow dbt
project's `macros/` folder (one file). The `sample_project/` here is a complete,
runnable reference.

## Supported features

- Table materializations (CREATE TABLE AS)
- View materializations (CREATE OR REPLACE VIEW)
- Incremental models
- Seeds
- Tests
- Sources
- Documentation generation

## Development

```bash
# Install in editable mode
pip install -e ".[dev]"

# Run adapter tests
python -m pytest tests/
```
