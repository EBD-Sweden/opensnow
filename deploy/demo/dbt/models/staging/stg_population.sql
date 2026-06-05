-- Quarterly population (national concept), thousands of persons.
select geo, year, sub_period as quarter, avg(value) as population_ths
from {{ source('eurostat_raw', 'namq_10_pe') }}
where na_item = 'POP_NC' and unit = 'THS_PER' and period_type = 'Q' and value is not null
group by geo, year, sub_period
