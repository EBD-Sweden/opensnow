-- Short-term money-market rate (3-month) — the "cash / deposit" return proxy.
select geo, year, sub_period as quarter, value as cash_rate_pct
from {{ source('eurostat_raw', 'irt_st_q') }}
where int_rt = 'IRT_M3' and period_type = 'Q' and value is not null
