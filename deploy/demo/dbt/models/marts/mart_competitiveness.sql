-- Competitiveness: unit labour cost vs real/nominal effective exchange rate (2015=100).
with reer as (
  select geo, year,
    round(avg(case when exch_rt = 'REER_IC42_CPI' then index_2015 end), 1) as reer_cpi_index,
    round(avg(case when exch_rt = 'NEER_IC42'      then index_2015 end), 1) as neer_index
  from {{ ref('stg_exch_rates') }} group by geo, year
)
select r.geo, r.year, u.ulc_index_2015, r.reer_cpi_index, r.neer_index
from reer r
left join {{ ref('stg_ulc') }} u on r.geo = u.geo and r.year = u.year
where u.ulc_index_2015 is not null or r.reer_cpi_index is not null
