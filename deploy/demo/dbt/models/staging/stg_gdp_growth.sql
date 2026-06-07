-- Real GDP growth, year-on-year % (seasonally adjusted) — the cross-country,
-- currency-neutral "real economy" return proxy (used instead of equity indices).
select geo, year, sub_period as quarter, value as gdp_real_yoy_pct
from {{ source('eurostat_raw', 'naidq_10_gdp') }}
where na_item = 'B1GQ' and unit = 'CLV_PCH_SM' and s_adj = 'SCA'
  and period_type = 'Q' and value is not null
