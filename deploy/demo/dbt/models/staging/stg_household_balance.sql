-- Household (S14) financial balance sheet, EUR millions, by instrument and side.
select geo, year, sub_period as quarter, finpos, na_item as instrument,
       value as value_mio_eur
from {{ source('eurostat_raw', 'nasq_10_f_bs') }}
where sector = 'S14' and unit = 'MIO_EUR' and period_type = 'Q' and value is not null
