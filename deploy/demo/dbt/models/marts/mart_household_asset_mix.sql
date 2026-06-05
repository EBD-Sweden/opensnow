-- Where households keep their money: composition of financial assets (% of total).
with t as (
  select geo, year, quarter,
    max(case when instrument = 'F'  then value_mio_eur end) as total,
    max(case when instrument = 'F2' then value_mio_eur end) as deposits,
    max(case when instrument = 'F3' then value_mio_eur end) as bonds,
    max(case when instrument = 'F5' then value_mio_eur end) as equity,
    max(case when instrument = 'F6' then value_mio_eur end) as insurance_pension
  from {{ ref('stg_household_balance') }}
  where finpos = 'ASS'
  group by geo, year, quarter
)
select geo, year, quarter, 'Deposits & cash'      as asset_class, round(deposits/nullif(total,0)*100, 1) as pct_of_assets from t
union all
select geo, year, quarter, 'Equity & fund shares' as asset_class, round(equity/nullif(total,0)*100, 1)   from t
union all
select geo, year, quarter, 'Insurance & pensions' as asset_class, round(insurance_pension/nullif(total,0)*100, 1) from t
union all
select geo, year, quarter, 'Bonds'                as asset_class, round(bonds/nullif(total,0)*100, 1)     from t
