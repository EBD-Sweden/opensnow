-- Government deficit/surplus (B9) and gross debt (GD), % of GDP, annual.
select geo, year, na_item, value as pct_gdp
from {{ source('eurostat_raw', 'gov_10dd_edpt1') }}
where period_type = 'A' and value is not null and na_item in ('B9','GD')
