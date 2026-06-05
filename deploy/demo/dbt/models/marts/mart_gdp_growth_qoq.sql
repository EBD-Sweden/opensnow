select geo, year, quarter, s_adj, gdp as gdp_qoq_pct
from {{ ref('stg_gdp') }}
where na_item = 'B1GQ' and unit = 'CLV_PCH_PRE'
