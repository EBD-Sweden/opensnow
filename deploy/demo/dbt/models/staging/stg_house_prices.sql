select geo, year, sub_period as quarter, unit, purchase, value as hpi
from {{ source('eurostat_raw', 'prc_hpi_q') }}
where value is not null
