-- Bank deposits & debt per capita, plus their share of household balance sheet.
with bal as (
  select geo, year, quarter,
    max(case when finpos = 'ASS'  and instrument = 'F2' then value_mio_eur end) as deposits_mio,
    max(case when finpos = 'ASS'  and instrument = 'F'  then value_mio_eur end) as assets_mio,
    max(case when finpos = 'LIAB' and instrument = 'F4' then value_mio_eur end) as loans_mio,
    max(case when finpos = 'LIAB' and instrument = 'F'  then value_mio_eur end) as liab_mio
  from {{ ref('stg_household_balance') }}
  group by geo, year, quarter
)
select b.geo, b.year, b.quarter,
  b.deposits_mio, b.loans_mio,
  round(b.deposits_mio / nullif(p.population_ths, 0) * 1000.0, 0) as deposits_per_capita_eur,
  round(b.loans_mio    / nullif(p.population_ths, 0) * 1000.0, 0) as debt_per_capita_eur,
  round(b.deposits_mio / nullif(b.assets_mio, 0) * 100.0, 1)      as deposits_pct_of_assets,
  round(b.loans_mio    / nullif(b.liab_mio, 0)   * 100.0, 1)      as loans_pct_of_liabilities,
  round((b.assets_mio - b.liab_mio) / nullif(p.population_ths, 0) * 1000.0, 0) as net_wealth_per_capita_eur
from bal b
left join {{ ref('stg_population') }} p
  on b.geo = p.geo and b.year = p.year and b.quarter = p.quarter
where b.deposits_mio is not null
