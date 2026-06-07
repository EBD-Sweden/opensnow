-- Household Investing Scorecard engine: real (inflation-adjusted) return of each
-- savings option, per country per year. GDP growth stands in for "the economy /
-- equity" (currency-neutral, all countries incl. Sweden).
with infl as (
  select geo, year, avg(headline_pct) as inflation from {{ ref('mart_inflation') }} group by geo, year
),
cash as (
  select geo, year, avg(cash_rate_pct) as cash_rate from {{ ref('stg_money_market') }} group by geo, year
),
-- Euro-area members report no national money-market rate (they share the euro
-- rate), so fall back to the EA aggregate for them.
ea_cash as (
  select year, avg(cash_rate_pct) as ea_cash from {{ ref('stg_money_market') }} where geo = 'EA' group by year
),
bond as (
  select geo, year, avg(rate_pct) as bond_yield
  from {{ ref('stg_interest_rates') }} where rate_type = 'long_10y' group by geo, year
),
house as (
  select geo, year, avg(yoy_pct) as house_growth from {{ ref('mart_house_price_yoy') }} group by geo, year
),
gdp as (
  select geo, year, avg(gdp_real_yoy_pct) as gdp_growth from {{ ref('stg_gdp_growth') }} group by geo, year
)
select i.geo, i.year,
  round(i.inflation, 1)                        as inflation_pct,
  round(coalesce(c.cash_rate, e.ea_cash), 2)   as cash_rate_pct,
  round(b.bond_yield, 2)                       as bond_yield_pct,
  round(h.house_growth, 1)                     as house_price_growth_pct,
  round(g.gdp_growth, 1)                       as gdp_real_growth_pct,
  round(coalesce(c.cash_rate, e.ea_cash) - i.inflation, 1) as real_cash_return_pct,
  round(b.bond_yield - i.inflation, 1)         as real_bond_return_pct,
  round(h.house_growth - i.inflation, 1)       as real_house_return_pct
from infl i
left join cash    c on i.geo = c.geo and i.year = c.year
left join ea_cash e on i.year = e.year
left join bond    b on i.geo = b.geo and i.year = b.year
left join house   h on i.geo = h.geo and i.year = h.year
left join gdp     g on i.geo = g.geo and i.year = g.year
