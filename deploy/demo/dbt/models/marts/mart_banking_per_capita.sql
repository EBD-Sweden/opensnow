-- How Europeans bank, per person — ALL countries (Eurostat household financial
-- accounts ÷ population), with card spend from ECB where available.
-- Deposits = F2 assets; consumer-credit proxy = F41 (short-term loans);
-- mortgage proxy = F42 (long-term loans); total debt = F4 liabilities.
with pop as (
  select geo, year, avg(population_ths) as pop_ths
  from {{ ref('stg_population') }} group by geo, year
),
latest_q as (
  select geo, year, max(quarter) as mq from {{ ref('stg_household_balance') }} group by geo, year
),
bal as (
  select b.geo, b.year,
    max(case when finpos='ASS'  and instrument='F2'  then value_mio_eur end) as deposits_meur,
    max(case when finpos='LIAB' and instrument='F41' then value_mio_eur end) as consumer_meur,
    max(case when finpos='LIAB' and instrument='F42' then value_mio_eur end) as mortgage_meur,
    max(case when finpos='LIAB' and instrument='F4'  then value_mio_eur end) as total_loans_meur
  from {{ ref('stg_household_balance') }} b
  join latest_q l on b.geo=l.geo and b.year=l.year and b.quarter=l.mq
  group by b.geo, b.year
),
cards as (
  select geo, year,
    coalesce(max(case when metric='credit_value' then value end),0)
      + coalesce(max(case when metric='debit_value' then value end),0) as card_spend_meur
  from {{ ref('stg_ecb_cards') }} group by geo, year
)
select b.geo, b.year,
  round(p.pop_ths/1000.0, 2) as population_mn,
  round(b.deposits_meur     / nullif(p.pop_ths,0) * 1000, 0) as deposits_per_capita_eur,
  round(b.consumer_meur     / nullif(p.pop_ths,0) * 1000, 0) as consumer_credit_per_capita_eur,
  round(b.mortgage_meur     / nullif(p.pop_ths,0) * 1000, 0) as mortgage_per_capita_eur,
  round(b.total_loans_meur  / nullif(p.pop_ths,0) * 1000, 0) as total_debt_per_capita_eur,
  round((b.deposits_meur - b.total_loans_meur) / nullif(p.pop_ths,0) * 1000, 0) as net_cash_position_per_capita_eur,
  round(c.card_spend_meur   / nullif(p.pop_ths,0) * 1000, 0) as card_spend_per_capita_eur,
  round(b.deposits_meur / nullif(b.total_loans_meur, 0), 2) as deposits_to_debt_ratio
from bal b
join pop p on b.geo=p.geo and b.year=p.year
left join cards c on b.geo=c.geo and b.year=c.year
