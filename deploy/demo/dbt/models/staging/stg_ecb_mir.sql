-- ECB MFI interest rates (MIR): rates on new household consumer credit & mortgages (% p.a.).
select geo, year, metric, rate as rate_pct
from {{ source('ecb_raw', 'ecb_mir') }}
where rate is not null
