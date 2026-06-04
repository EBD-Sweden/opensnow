{# OpenSnow requires a bare, unqualified, unquoted relation name everywhere
   (no catalog/schema, no quotes). Force that explicitly so it holds even for the
   model's own `this` relation, whose policy can otherwise leak the schema. #}
{% macro opensnow__create_table_as(temporary, relation, sql) -%}
  {%- set tmp = "temporary " if temporary else "" -%}
  create {{ tmp }}table {{ relation.include(database=false, schema=false) }}
  as (
    {{ sql }}
  );
{%- endmacro %}


{% macro opensnow__create_view_as(relation, sql) -%}
  create or replace view {{ relation.include(database=false, schema=false) }}
  as (
    {{ sql }}
  );
{%- endmacro %}


{% macro opensnow__drop_relation(relation) -%}
  {% call statement('drop_relation', auto_begin=False) %}
    {%- if relation.type == 'view' -%}
      drop view if exists {{ relation.include(database=false, schema=false) }}
    {%- else -%}
      drop table if exists {{ relation.include(database=false, schema=false) }}
    {%- endif -%}
  {% endcall %}
{%- endmacro %}


{% macro opensnow__rename_relation(from_relation, to_relation) -%}
  {% call statement('rename_relation') %}
    alter table {{ from_relation }} rename to {{ to_relation }}
  {% endcall %}
{%- endmacro %}


{% macro opensnow__list_relations_without_caching(schema_relation) -%}
  {% call statement('list_relations_without_caching', fetch_result=True) %}
    select
      '{{ schema_relation.database }}' as database,
      table_name as name,
      table_schema as schema,
      case
        when table_type = 'BASE TABLE' then 'table'
        when table_type = 'VIEW' then 'view'
        else table_type
      end as type
    from information_schema.tables
    where table_schema = '{{ schema_relation.schema }}'
      {% if schema_relation.database %}
        and table_catalog = '{{ schema_relation.database }}'
      {% endif %}
    order by table_schema, table_name
  {% endcall %}
  {{ return(load_result('list_relations_without_caching').table) }}
{%- endmacro %}


{% macro opensnow__get_columns_in_relation(relation) -%}
  {% call statement('get_columns_in_relation', fetch_result=True) %}
    select
      column_name,
      data_type,
      character_maximum_length,
      numeric_precision,
      numeric_scale
    from information_schema.columns
    where table_name = '{{ relation.identifier }}'
      and table_schema = '{{ relation.schema }}'
      {% if relation.database %}
        and table_catalog = '{{ relation.database }}'
      {% endif %}
    order by ordinal_position
  {% endcall %}
  {{ return(load_result('get_columns_in_relation').table) }}
{%- endmacro %}


{% macro opensnow__current_timestamp() -%}
  current_timestamp()
{%- endmacro %}


{% macro opensnow__make_temp_relation(base_relation, suffix) %}
  {% set tmp_identifier = base_relation.identifier ~ suffix %}
  {% do return(base_relation.incorporate(
    path={"identifier": tmp_identifier, "schema": base_relation.schema}
  )) %}
{% endmacro %}


{# OpenSnow does not expose pg_catalog dependency views; relation-dependency
   detection (used for cascade) is not needed for the flat model graph. #}
{% macro opensnow__get_relations() -%}
  {{ return([]) }}
{%- endmacro %}
