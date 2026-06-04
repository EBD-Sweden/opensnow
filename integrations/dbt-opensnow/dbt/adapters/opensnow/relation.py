from dataclasses import dataclass, field

from dbt.adapters.base.relation import BaseRelation
from dbt.adapters.contracts.relation import Policy


@dataclass
class OpenSnowIncludePolicy(Policy):
    # OpenSnow uses a single catalog schema and its CREATE TABLE AS target must
    # be a bare, unqualified identifier — so never render database/schema.
    database: bool = False
    schema: bool = False
    identifier: bool = True


@dataclass
class OpenSnowQuotePolicy(Policy):
    # OpenSnow's engine rejects quoted CREATE TABLE targets; emit bare names.
    database: bool = False
    schema: bool = False
    identifier: bool = False


@dataclass(frozen=True, eq=False, repr=False)
class OpenSnowRelation(BaseRelation):
    quote_policy: OpenSnowQuotePolicy = field(default_factory=OpenSnowQuotePolicy)
    include_policy: OpenSnowIncludePolicy = field(default_factory=OpenSnowIncludePolicy)
