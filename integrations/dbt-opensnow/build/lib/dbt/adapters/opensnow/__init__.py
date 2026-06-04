from dbt.adapters.opensnow.connections import OpenSnowConnectionManager  # noqa: F401
from dbt.adapters.opensnow.connections import OpenSnowCredentials  # noqa: F401
from dbt.adapters.opensnow.impl import OpenSnowAdapter  # noqa: F401
from dbt.adapters.opensnow.column import OpenSnowColumn  # noqa: F401

from dbt.adapters.base import AdapterPlugin
from dbt.include import opensnow

Plugin = AdapterPlugin(
    adapter=OpenSnowAdapter,
    credentials=OpenSnowCredentials,
    include_path=opensnow.PACKAGE_PATH,
)
