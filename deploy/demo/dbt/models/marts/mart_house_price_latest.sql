with ranked as (
  select geo, year, quarter, hpi as hpi_2015_100,
         row_number() over (partition by geo order by year desc, quarter desc) as rn
  from {{ ref('stg_house_prices') }}
  where unit = 'I15_Q' and purchase = 'TOTAL'
)
select geo, year, quarter, hpi_2015_100 from ranked where rn = 1
