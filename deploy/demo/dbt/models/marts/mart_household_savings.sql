-- Household saving rate, real disposable income per capita, net financial wealth.
select geo, year, quarter,
  max(case when indicator = 'saving_rate'           then value end) as saving_rate_pct,
  max(case when indicator = 'real_income_pc_idx'    then value end) as real_income_pc_idx_2010,
  max(case when indicator = 'real_income_pc_growth' then value end) as real_income_pc_growth_pct,
  max(case when indicator = 'net_fin_wealth'        then value end) as net_fin_wealth_pct_income,
  max(case when indicator = 'investment_rate'       then value end) as investment_rate_pct
from {{ ref('stg_household_kpi') }}
group by geo, year, quarter
