select geo, year, sub_period as quarter, s_adj, unit, na_item, value as gdp
from {{ source('eurostat_raw', 'naidq_10_gdp') }}
where value is not null
