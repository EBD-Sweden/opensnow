-- Real household credit (ECB BSI + MIR): consumer credit & mortgages, stocks and rates.
with bsi as (
  select geo, year,
    max(case when metric='consumer_credit' then value_mio_eur end) as consumer_credit_meur,
    max(case when metric='house_purchase'  then value_mio_eur end) as mortgages_meur,
    max(case when metric='deposits'        then value_mio_eur end) as deposits_meur
  from {{ ref('stg_ecb_bsi') }} group by geo, year
),
mir as (
  select geo, year,
    max(case when metric='consumer_rate' then rate_pct end) as consumer_rate_pct,
    max(case when metric='mortgage_rate' then rate_pct end) as mortgage_rate_pct
  from {{ ref('stg_ecb_mir') }} group by geo, year
)
select b.geo, b.year,
  b.consumer_credit_meur, b.mortgages_meur, b.deposits_meur,
  m.consumer_rate_pct, m.mortgage_rate_pct,
  round(b.consumer_credit_meur / nullif(b.consumer_credit_meur + b.mortgages_meur, 0) * 100, 1) as consumer_share_of_credit_pct,
  round(m.consumer_rate_pct - m.mortgage_rate_pct, 2) as consumer_minus_mortgage_spread_pct
from bsi b
left join mir m on b.geo = m.geo and b.year = m.year
