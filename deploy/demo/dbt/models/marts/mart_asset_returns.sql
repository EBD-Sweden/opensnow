-- Household Investing Scorecard engine: real (inflation-adjusted) return of each
-- savings option, per country per year. The equity / fund return is the YoY
-- change of each country's own stock-market index (DAX, OMXS30, …); GDP growth
-- is kept alongside as an "economy" reference only.
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
),
-- Equity: total return = YoY % change of each country's year-end stock-market
-- index plus its assumed dividend yield (0 where the index is already
-- total-return, e.g. the DAX), so all countries are on a fund-investor footing.
equity as (
  select e.geo, e.year,
    100.0 * (e.index_level / p.index_level - 1) + e.div_yield_pct as equity_yoy
  from {{ ref('stg_equity_index') }} e
  join {{ ref('stg_equity_index') }} p on e.geo = p.geo and p.year = e.year - 1
),
-- Euro-area members without a national index fall back to the EURO STOXX 50.
ea_equity as (
  select year, equity_yoy as ea_equity_yoy from equity where geo = 'EA'
)
select i.geo, i.year,
  round(i.inflation, 1)                        as inflation_pct,
  round(coalesce(c.cash_rate, e.ea_cash), 2)   as cash_rate_pct,
  round(b.bond_yield, 2)                       as bond_yield_pct,
  round(h.house_growth, 1)                     as house_price_growth_pct,
  round(g.gdp_growth, 1)                       as gdp_real_growth_pct,
  round(coalesce(q.equity_yoy, eq.ea_equity_yoy), 1)              as equity_index_yoy_pct,
  round(coalesce(c.cash_rate, e.ea_cash) - i.inflation, 1)        as real_cash_return_pct,
  round(b.bond_yield - i.inflation, 1)                            as real_bond_return_pct,
  round(h.house_growth - i.inflation, 1)                          as real_house_return_pct,
  round(coalesce(q.equity_yoy, eq.ea_equity_yoy) - i.inflation, 1) as real_equity_return_pct
from infl i
left join cash      c  on i.geo = c.geo and i.year = c.year
left join ea_cash   e  on i.year = e.year
left join bond      b  on i.geo = b.geo and i.year = b.year
left join house     h  on i.geo = h.geo and i.year = h.year
left join gdp       g  on i.geo = g.geo and i.year = g.year
left join equity    q  on i.geo = q.geo and i.year = q.year
left join ea_equity eq on i.year = eq.year
