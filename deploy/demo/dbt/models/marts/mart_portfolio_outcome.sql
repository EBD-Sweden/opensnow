-- The verdict: weight each country's household asset mix by the real return of
-- each asset class -> implied real portfolio return, compounded into a real
-- wealth index (base 100 in the first year). Answers "is the fund-heavy mix
-- the smart one?" head-to-head.
--
-- Proxies (documented, approximate): equity & funds -> real return of the
-- country's own stock-market index (DAX, OMXS30, …); insurance & pensions ->
-- 50% bonds + 50% equity (balanced); cash -> real money-market return; bonds ->
-- real bond yield.
with mix as (
  select geo, year,
    avg(case when asset_class = 'Deposits & cash'      then pct_of_assets end) as w_cash,
    avg(case when asset_class = 'Equity & fund shares' then pct_of_assets end) as w_equity,
    avg(case when asset_class = 'Insurance & pensions' then pct_of_assets end) as w_pension,
    avg(case when asset_class = 'Bonds'                then pct_of_assets end) as w_bonds
  from {{ ref('mart_household_asset_mix') }} group by geo, year
),
joined as (
  select m.geo, m.year,
    round(m.w_cash, 1)   as cash_share_pct,
    round(m.w_equity, 1) as equity_share_pct,
    round(
      ( m.w_cash    * r.real_cash_return_pct
      + m.w_equity  * r.real_equity_return_pct
      + m.w_pension * (0.5 * r.real_bond_return_pct + 0.5 * r.real_equity_return_pct)
      + m.w_bonds   * r.real_bond_return_pct )
      / nullif(m.w_cash + m.w_equity + m.w_pension + m.w_bonds, 0)
    , 1) as implied_real_return_pct
  from mix m
  join {{ ref('mart_asset_returns') }} r on m.geo = r.geo and m.year = r.year
  where r.real_cash_return_pct is not null
    and r.real_equity_return_pct is not null
    and r.real_bond_return_pct is not null
    and m.year >= 2015
)
select geo, year, cash_share_pct, equity_share_pct, implied_real_return_pct,
  round(100 * exp(sum(ln(1 + implied_real_return_pct / 100.0))
    over (partition by geo order by year rows between unbounded preceding and current row)), 1) as real_wealth_index
from joined
