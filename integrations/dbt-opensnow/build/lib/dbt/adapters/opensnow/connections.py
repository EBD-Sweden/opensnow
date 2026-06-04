from dataclasses import dataclass
from typing import Optional

import psycopg2

from dbt.adapters.postgres.connections import PostgresConnectionManager, PostgresCredentials
from dbt_common.exceptions import DbtDatabaseError
from dbt.adapters.contracts.connection import AdapterResponse, Connection


@dataclass
class OpenSnowCredentials(PostgresCredentials):
    """Credentials for connecting to OpenSnow via its PostgreSQL wire protocol."""

    host: str = "localhost"
    port: int = 5433
    user: str = "opensnow"
    password: str = ""
    dbname: str = "opensnow"
    schema: str = "public"
    keepalives_idle: int = 0
    connect_timeout: int = 10
    retries: int = 1
    search_path: Optional[str] = None
    role: Optional[str] = None
    sslmode: Optional[str] = None
    sslcert: Optional[str] = None
    sslkey: Optional[str] = None
    sslrootcert: Optional[str] = None

    @property
    def type(self):
        return "opensnow"

    @property
    def unique_field(self):
        return self.host

    def _connection_keys(self):
        return (
            "host",
            "port",
            "user",
            "dbname",
            "schema",
            "connect_timeout",
            "role",
            "search_path",
            "sslmode",
        )


class OpenSnowConnectionManager(PostgresConnectionManager):
    """Connection manager for OpenSnow, using psycopg2 over the PG wire protocol."""

    TYPE = "opensnow"

    @classmethod
    def open(cls, connection: Connection) -> Connection:
        if connection.state == "open":
            return connection

        credentials = cls.get_credentials(connection.credentials)

        kwargs = {
            "host": credentials.host,
            "port": credentials.port,
            "user": credentials.user,
            "password": credentials.password,
            "dbname": credentials.dbname,
            "connect_timeout": credentials.connect_timeout,
        }

        if credentials.sslmode:
            kwargs["sslmode"] = credentials.sslmode
        if credentials.sslcert:
            kwargs["sslcert"] = credentials.sslcert
        if credentials.sslkey:
            kwargs["sslkey"] = credentials.sslkey
        if credentials.sslrootcert:
            kwargs["sslrootcert"] = credentials.sslrootcert

        if credentials.search_path:
            kwargs["options"] = f"-c search_path={credentials.search_path}"

        if credentials.keepalives_idle:
            kwargs["keepalives_idle"] = credentials.keepalives_idle

        try:
            handle = psycopg2.connect(**kwargs)
            handle.set_session(autocommit=True)

            if credentials.role:
                cursor = handle.cursor()
                cursor.execute(f"SET ROLE {credentials.role}")
                cursor.close()

            connection.handle = handle
            connection.state = "open"
        except psycopg2.Error as e:
            connection.handle = None
            connection.state = "fail"
            raise DbtDatabaseError(str(e)) from e

        return connection

    @classmethod
    def get_credentials(cls, credentials: OpenSnowCredentials) -> OpenSnowCredentials:
        return credentials

    def cancel(self, connection: Connection):
        """Cancel the current query on the given connection."""
        connection_name = connection.name
        try:
            pid = connection.handle.get_backend_pid()
            sql = f"SELECT pg_cancel_backend({pid})"
            _, cursor = self.add_query(sql)
            res = cursor.fetchone()
            return res
        except Exception:
            pass

    @classmethod
    def get_response(cls, cursor) -> AdapterResponse:
        message = str(cursor.statusmessage)
        rows = cursor.rowcount
        return AdapterResponse(
            _message=message,
            rows_affected=rows,
            code=message,
        )
