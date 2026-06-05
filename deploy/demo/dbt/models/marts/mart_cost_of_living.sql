-- The household squeeze: real income growth vs inflation vs house-price growth, yearly.
with infl as (
  select geo, year, round(avg(headline_pct), 1) as inflation_pct
  from {{ ref('mart_inflation') }} group by geo, year
),
income as (
  select geo, year, round(avg(value), 1) as real_income_growth_pct
  from {{ ref('stg_household_kpi') }} where indicator = 'real_income_pc_growth' group by geo, year
),
hp as (
  select geo, year, round(avg(yoy_pct), 1) as house_price_yoy_pct
  from {{ ref('mart_house_price_yoy') }} group by geo, year
)
select f.geo, f.year, f.inflation_pct, i.real_income_growth_pct, h.house_price_yoy_pct
from infl f
left join income i on f.geo = i.geo and f.year = i.year
left join hp h     on f.geo = h.geo and f.year = h.year
