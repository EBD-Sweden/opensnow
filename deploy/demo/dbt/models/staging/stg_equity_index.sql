-- National stock-market index, year-end level, per country (from FMP).
-- The equity / fund benchmark for the Investing Scorecard, replacing the older
-- GDP-growth proxy. Note: index dividend treatment differs by exchange (e.g.
-- DAX is total-return, OMXS30 price-only), so cross-country *levels* are not
-- strictly comparable — the scorecard relies on each country's own YoY change
-- weighted by its own asset mix, not on one index out-returning another.
select geo, year, value as index_level, div_yield_pct
from {{ source('market_raw', 'equity_index') }}
where value is not null
