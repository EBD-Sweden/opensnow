-- Sovereign risk: government debt-to-GDP, deficit, and 10y bond yield together.
with debt as (
  select geo, year, quarter, debt_pct_gdp from {{ ref('stg_gov_debt') }}
),
yld as (
  select geo, year, quarter, rate_pct as bond_yield_10y_pct
  from {{ ref('stg_interest_rates') }} where rate_type = 'long_10y'
),
deficit as (
  select geo, year, max(case when na_item = 'B9' then pct_gdp end) as deficit_pct_gdp
  from {{ ref('stg_gov_deficit') }} group by geo, year
)
select d.geo, d.year, d.quarter, d.debt_pct_gdp,
       y.bond_yield_10y_pct, f.deficit_pct_gdp
from debt d
left join yld y on d.geo = y.geo and d.year = y.year and d.quarter = y.quarter
left join deficit f on d.geo = f.geo and d.year = f.year
