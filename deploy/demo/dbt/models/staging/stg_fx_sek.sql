-- SEK per EUR (quarterly average) — to compare the Swedish krona against the euro.
select geo, year, quarter, value as sek_per_eur
from {{ source('eurostat_raw', 'ert_sek_eur') }}
where value is not null
