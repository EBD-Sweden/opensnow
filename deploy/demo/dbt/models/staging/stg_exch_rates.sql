-- Effective exchange rates (vs 42 trading partners), index 2015=100, quarterly.
select geo, year, sub_period as quarter, exch_rt, value as index_2015
from {{ source('eurostat_raw', 'ert_eff_ic_q') }}
where period_type = 'Q' and value is not null
