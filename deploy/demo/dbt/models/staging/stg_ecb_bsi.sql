-- ECB MFI balance sheet (BSI): household consumer credit, mortgages, deposits (EUR mn, year-end).
select geo, year, metric, value as value_mio_eur
from {{ source('ecb_raw', 'ecb_bsi') }}
where value is not null
