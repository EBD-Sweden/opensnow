select geo, year, quarter, hpi as yoy_pct
from {{ ref('stg_house_prices') }}
where unit = 'RCH_A' and purchase = 'TOTAL'
