-- Household (S14_S15) key indicators: saving rate, real income per capita, wealth.
select geo, year, sub_period as quarter,
  case na_item
    when 'SRG_S14_S15'    then 'saving_rate'
    when 'B6G_R_HAB_2010' then 'real_income_pc_idx'
    when 'B6G_R_HAB_GR'   then 'real_income_pc_growth'
    when 'NFW_S14_S15'    then 'net_fin_wealth'
    when 'IRG_S14_S15'    then 'investment_rate'
  end as indicator,
  value
from {{ source('eurostat_raw', 'nasq_10_ki') }}
where period_type = 'Q' and value is not null
  and na_item in ('SRG_S14_S15','B6G_R_HAB_2010','B6G_R_HAB_GR','NFW_S14_S15','IRG_S14_S15')
