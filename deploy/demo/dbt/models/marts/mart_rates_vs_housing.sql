-- Monetary transmission: short/long rates alongside house prices and household debt.
with rates as (
  select geo, year, quarter,
    max(case when rate_type = 'short_3m' then rate_pct end) as short_rate_3m_pct,
    max(case when rate_type = 'long_10y' then rate_pct end) as long_rate_10y_pct
  from {{ ref('stg_interest_rates') }} group by geo, year, quarter
)
select h.geo, h.year, h.quarter, h.hpi_2015_100,
  r.short_rate_3m_pct, r.long_rate_10y_pct, d.debt_per_capita_eur
from {{ ref('mart_house_price_index') }} h
left join rates r on h.geo = r.geo and h.year = r.year and h.quarter = r.quarter
left join {{ ref('mart_household_banking') }} d on h.geo = d.geo and h.year = d.year and h.quarter = d.quarter
