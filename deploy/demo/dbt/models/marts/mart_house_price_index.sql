select geo, year, quarter, hpi as hpi_2015_100
from {{ ref('stg_house_prices') }}
where unit = 'I15_Q' and purchase = 'TOTAL'
