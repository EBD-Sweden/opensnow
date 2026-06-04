from typing import List, Optional, Set

from dbt.adapters.postgres.impl import PostgresAdapter
from dbt.adapters.opensnow.connections import OpenSnowConnectionManager
from dbt.adapters.opensnow.column import OpenSnowColumn

from dbt.adapters.base.relation import BaseRelation
from dbt_common.utils import AttrDict


class OpenSnowAdapter(PostgresAdapter):
    """dbt adapter implementation for OpenSnow.

    OpenSnow exposes a PostgreSQL wire protocol on port 5433. This adapter
    extends the PostgresAdapter and overrides behaviour where OpenSnow's SQL
    dialect differs from PostgreSQL.
    """

    ConnectionManager = OpenSnowConnectionManager
    Column = OpenSnowColumn

    @classmethod
    def date_function(cls) -> str:
        return "current_date()"

    def list_relations_without_caching(
        self, schema_relation: BaseRelation
    ) -> List[BaseRelation]:
        """Query information_schema to discover tables and views in the given schema."""
        kwargs = {"schema_relation": schema_relation}
        results = self.execute_macro("opensnow__list_relations_without_caching", kwargs=kwargs)

        relations = []
        for row in results:
            if isinstance(row, AttrDict):
                row_dict = dict(row)
            else:
                row_dict = dict(zip(row.keys(), row))

            relations.append(
                self.Relation.create(
                    database=row_dict.get("database"),
                    schema=row_dict.get("schema"),
                    identifier=row_dict.get("name"),
                    type=row_dict.get("type"),
                )
            )

        return relations

    def get_columns_in_relation(self, relation: BaseRelation) -> List[OpenSnowColumn]:
        """Query information_schema to get column metadata for a relation."""
        kwargs = {"relation": relation}
        results = self.execute_macro("opensnow__get_columns_in_relation", kwargs=kwargs)

        columns = []
        for row in results:
            if isinstance(row, AttrDict):
                row_dict = dict(row)
            else:
                row_dict = dict(zip(row.keys(), row))

            column = self.Column(
                column=row_dict.get("column_name"),
                dtype=row_dict.get("data_type"),
                char_size=row_dict.get("character_maximum_length"),
                numeric_precision=row_dict.get("numeric_precision"),
                numeric_scale=row_dict.get("numeric_scale"),
            )
            columns.append(column)

        return columns

    def verify_database(self, database: str) -> str:
        """OpenSnow may not support cross-database queries; just return the database name."""
        return database

    @classmethod
    def is_cancelable(cls) -> bool:
        return True
