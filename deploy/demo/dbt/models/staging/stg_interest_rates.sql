-- Short-term money-market (3-month) and long-term (10y Maastricht) rates, unified.
select geo, year, sub_period as quarter, 'short_3m' as rate_type, value as rate_pct
from {{ source('eurostat_raw', 'irt_st_q') }}
where int_rt = 'IRT_M3' and period_type = 'Q' and value is not null
union all
select geo, year, sub_period as quarter, 'long_10y' as rate_type, value as rate_pct
from {{ source('eurostat_raw', 'irt_lt_mcby_q') }}
where int_rt = 'MCBY' and period_type = 'Q' and value is not null
