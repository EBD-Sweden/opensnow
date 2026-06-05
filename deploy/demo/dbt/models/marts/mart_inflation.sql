-- HICP annual inflation: headline plus energy / food / services breakdown.
select geo, year, month,
  max(case when coicop = 'CP00' then annual_rate_pct end) as headline_pct,
  max(case when coicop = 'NRG'  then annual_rate_pct end) as energy_pct,
  max(case when coicop = 'FOOD' then annual_rate_pct end) as food_pct,
  max(case when coicop = 'SERV' then annual_rate_pct end) as services_pct
from {{ ref('stg_inflation') }}
group by geo, year, month
