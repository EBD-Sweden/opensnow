-- Real card usage (ECB PSS): credit vs debit card payments + ATM cash withdrawals.
with p as (
  select geo, year,
    max(case when metric='credit_count' then value end) as credit_count_mn,
    max(case when metric='credit_value' then value end) as credit_value_meur,
    max(case when metric='debit_count'  then value end) as debit_count_mn,
    max(case when metric='debit_value'  then value end) as debit_value_meur,
    max(case when metric='atm_count'    then value end) as atm_count_mn,
    max(case when metric='atm_value'    then value end) as atm_value_meur
  from {{ ref('stg_ecb_cards') }} group by geo, year
)
select geo, year,
  credit_count_mn, credit_value_meur, debit_count_mn, debit_value_meur,
  atm_count_mn, atm_value_meur,
  round(credit_value_meur / nullif(credit_count_mn, 0), 2) as avg_credit_txn_eur,
  round(debit_value_meur  / nullif(debit_count_mn, 0), 2)  as avg_debit_txn_eur,
  round(atm_value_meur    / nullif(atm_count_mn, 0), 2)    as avg_atm_withdrawal_eur,
  round(credit_count_mn / nullif(credit_count_mn + debit_count_mn, 0) * 100, 1) as credit_share_of_card_txns_pct
from p
