from dbt.adapters.postgres.column import PostgresColumn


class OpenSnowColumn(PostgresColumn):
    """Column representation for the OpenSnow adapter.

    Extends PostgresColumn. Override type mappings here if OpenSnow's type
    system diverges from PostgreSQL.
    """

    @classmethod
    def translate_type(cls, dtype: str) -> str:
        """Translate OpenSnow types to dbt's canonical types."""
        # OpenSnow follows the PostgreSQL type system via its PG wire protocol.
        # Add overrides here if OpenSnow introduces custom types.
        return super().translate_type(dtype)

    @classmethod
    def string_type(cls, string_length: int) -> str:
        return f"varchar({string_length})"

    @property
    def data_type(self) -> str:
        if self.is_string():
            return self.string_type(self.string_size())
        elif self.is_numeric():
            return self.numeric_type(self.dtype, self.numeric_precision, self.numeric_scale)
        else:
            return self.dtype
