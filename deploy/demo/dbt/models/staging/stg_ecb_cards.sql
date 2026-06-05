-- ECB payment statistics (PSS): card payments + ATM cash withdrawals, annual.
select geo, year, metric, value
from {{ source('ecb_raw', 'ecb_cards') }}
where value is not null
