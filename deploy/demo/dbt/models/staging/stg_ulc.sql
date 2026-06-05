-- Nominal unit labour cost per hour worked, index 2015=100, annual.
select geo, year, value as ulc_index_2015
from {{ source('eurostat_raw', 'nama_10_lp_ulc') }}
where period_type = 'A' and value is not null
