{#
  OpenSnow table materialization.

  OpenSnow's engine supports `CREATE TABLE AS SELECT` and `DROP TABLE`, but not
  the temp-table + `ALTER TABLE ... RENAME` atomic-swap that dbt-postgres uses by
  default. This materialization therefore does a straightforward
  drop-if-exists → create-table-as. Transaction/COMMIT calls are emitted by the
  hooks but are acknowledged as no-ops by OpenSnow's pgwire trusted mode.
#}
{% materialization table, adapter='opensnow', supported_languages=['sql'] %}

  {%- set target_relation = this.incorporate(type='table') -%}

  {{ run_hooks(pre_hooks, inside_transaction=False) }}
  {{ run_hooks(pre_hooks, inside_transaction=True) }}

  {# Unconditional drop-if-exists: cache-independent, so a re-run works even
     when the relation cache hasn't observed the existing table. #}
  {% call statement('drop_target') %}
    drop table if exists {{ target_relation.identifier }}
  {% endcall %}

  {% call statement('main') -%}
    {{ create_table_as(False, target_relation, compiled_code) }}
  {%- endcall %}

  {{ run_hooks(post_hooks, inside_transaction=True) }}
  {{ adapter.commit() }}
  {{ run_hooks(post_hooks, inside_transaction=False) }}

  {% do persist_docs(target_relation, model) %}

  {{ return({'relations': [target_relation]}) }}

{% endmaterialization %}
