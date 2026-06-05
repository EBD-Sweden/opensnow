-- General government gross debt, % of GDP, quarterly.
select geo, year, sub_period as quarter, value as debt_pct_gdp
from {{ source('eurostat_raw', 'gov_10q_ggdebt') }}
where period_type = 'Q' and value is not null
