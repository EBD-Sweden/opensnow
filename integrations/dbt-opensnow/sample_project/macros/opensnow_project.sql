{#
  Required project macros for dbt on OpenSnow.

  OpenSnow's engine differs from PostgreSQL in two ways that dbt must adapt to:
    1. `CREATE TABLE AS` targets must be a BARE, unqualified, unquoted name
       (no catalog/schema, no quotes).
    2. There is no `ALTER TABLE ... RENAME`, so the default table materialization
       (temp table + atomic rename swap) cannot be used.

  dbt (>= 1.8) does not apply an *imported package's* materialization or
  dispatched-macro overrides by default, so these must live in the dbt PROJECT's
  own `macros/` directory (copy this file into every OpenSnow dbt project). The
  OpenSnow server must run with trusted SQL: `--enable-pgwire OPENSNOW_TRUSTED_SQL=1`.
#}

{#- Bare-name CREATE TABLE AS (renders just the identifier). -#}
{% macro opensnow__create_table_as(temporary, relation, sql) -%}
  create table {{ relation.identifier }} as (
    {{ sql }}
  )
{%- endmacro %}

{% macro opensnow__drop_relation(relation) -%}
  {% call statement('drop_relation', auto_begin=False) %}
    drop table if exists {{ relation.identifier }}
  {% endcall %}
{%- endmacro %}

{#- Table materialization: unconditional drop-if-exists + CREATE TABLE AS.
    Avoids the temp-table + ALTER RENAME swap OpenSnow does not support. #}
{% materialization table, adapter='opensnow', supported_languages=['sql'] %}

  {%- set target_relation = this.incorporate(type='table') -%}

  {{ run_hooks(pre_hooks, inside_transaction=False) }}
  {{ run_hooks(pre_hooks, inside_transaction=True) }}

  {% call statement('drop_target') %}
    drop table if exists {{ target_relation.identifier }}
  {% endcall %}

  {% call statement('main') -%}
    {{ opensnow__create_table_as(False, target_relation, compiled_code) }}
  {%- endcall %}

  {{ run_hooks(post_hooks, inside_transaction=True) }}
  {{ adapter.commit() }}
  {{ run_hooks(post_hooks, inside_transaction=False) }}

  {% do persist_docs(target_relation, model) %}

  {{ return({'relations': [target_relation]}) }}

{% endmaterialization %}
