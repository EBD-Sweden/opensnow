-- HICP annual rate of change (monthly), headline + key special aggregates.
select geo, year, sub_period as month, coicop, value as annual_rate_pct
from {{ source('eurostat_raw', 'prc_hicp_manr') }}
where period_type = 'M' and value is not null
