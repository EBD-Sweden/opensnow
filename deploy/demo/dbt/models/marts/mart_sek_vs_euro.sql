-- Sweden (SEK) vs the euro area: money-market & bond rates side by side, plus the
-- SEK/EUR exchange rate, per year.
with cash as (
  select year,
    avg(case when geo = 'SE' then cash_rate_pct end) as se_cash_rate_pct,
    avg(case when geo = 'EA' then cash_rate_pct end) as euro_cash_rate_pct
  from {{ ref('stg_money_market') }} where geo in ('SE', 'EA') group by year
),
bond as (
  select year,
    avg(case when geo = 'SE' then rate_pct end)         as se_bond_yield_pct,
    avg(case when geo = 'EU27_2020' then rate_pct end)  as eu_bond_yield_pct
  from {{ ref('stg_interest_rates') }} where rate_type = 'long_10y' and geo in ('SE', 'EU27_2020') group by year
),
fx as (
  select year, round(avg(sek_per_eur), 2) as sek_per_eur from {{ ref('stg_fx_sek') }} group by year
)
select c.year,
  round(c.se_cash_rate_pct, 2)  as se_cash_rate_pct,
  round(c.euro_cash_rate_pct, 2) as euro_cash_rate_pct,
  round(b.se_bond_yield_pct, 2)  as se_bond_yield_pct,
  round(b.eu_bond_yield_pct, 2)  as eu_bond_yield_pct,
  f.sek_per_eur
from cash c
left join bond b on c.year = b.year
left join fx f on c.year = f.year
order by c.year
