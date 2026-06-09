#!/usr/bin/env python3
"""Build the cross-country credit & spending analysis tables from the raw
source Parquet (SCB, SSB, BIS, ECB, Eurostat HBS) using DuckDB, and emit CSVs.

These power the "How Europe Borrows & Spends — a step-by-step read" dashboard.
They are loaded into the Postgres serving DB (schema `eurostat`, prefix
`analysis_`) for Metabase, e.g.:

    python3 tools/build_credit_analysis.py sample-data /tmp/analysis
    for t in debt_levels credit_mix se_mortgage_ts spending se_spending; do
      psql -c "\\copy eurostat.analysis_$t from '/tmp/analysis/analysis_$t.csv' csv header"
    done

Step 1 debt_levels  : household debt % of GDP, comparable across countries (BIS)
Step 2 credit_mix   : mortgage vs consumer % of household loans (SE=SCB, NO=SSB, euro=ECB)
Step 3 se_mortgage_ts: Sweden mortgage stock + mortgage rate over time (SCB)
Step 4 spending     : consumption structure by COICOP, by country (Eurostat HBS 2015)
Step 5 se_spending  : Swedish household expenditure by category (SCB budget survey 2021)
"""
import sys
from pathlib import Path

import duckdb


def main(argv):
    src = Path(argv[0]) if argv else Path("sample-data")
    out = Path(argv[1]) if len(argv) > 1 else Path(".")
    out.mkdir(parents=True, exist_ok=True)
    con = duckdb.connect()
    for f in src.glob("*.parquet"):
        con.execute(f"create view {f.stem} as select * from read_parquet('{f.as_posix()}')")

    def dump(name, sql):
        con.execute(f"copy ({sql}) to '{(out / f'{name}.csv').as_posix()}' (header)")
        print(f"  {name}.csv")

    dump("analysis_debt_levels", """
        select geo, round(household_debt_pct_gdp,1) debt_pct_gdp from bis_household_credit
        qualify row_number() over (partition by geo order by year desc, quarter desc)=1
        order by debt_pct_gdp desc""")

    dump("analysis_credit_mix", """
      with se as (select 'SE' geo, 'SCB (collateral)' src,
         round(100.0*sum(case when collateral in ('single-family dwellings','condominiums','tenant-owner apartments','multi-dwelling building') then value end)/sum(case when collateral='total for all collateral' then value end),1) mortgage_pct,
         round(100.0*sum(case when collateral='unsecured credits' then value end)/sum(case when collateral='total for all collateral' then value end),1) consumer_pct
         from scb_household_loans_by_collateral where counterparty_sector='households' and monetary_financial_institutions_mfi='MFI' and month=12 and year=(select max(year) from scb_household_loans_by_collateral where month=12)),
      no as (select 'NO' geo, 'SSB (loan type)' src,
         round(100.0*sum(case when trim(type_of_loans)='Loans secured on dwellings in total' then value end)/sum(case when type_of_loans='Total loans' then value end),1),
         round(100.0*sum(case when trim(type_of_loans)='Other repayment loans' then value end)/sum(case when type_of_loans='Total loans' then value end),1)
         from ssb_norway_household_loans_and_rates where sector='Households' and financial_corporation like 'Total%' and fixed_interest_period='Total' and measure='Loans (NOK million)' and period=(select max(period) from ssb_norway_household_loans_and_rates)),
      eu as (select geo, 'ECB' src, round(100.0*mort/(mort+cons),1), round(100.0*cons/(mort+cons),1) from
         (select geo, max(case when metric='house_purchase' then value end) mort, max(case when metric='consumer_credit' then value end) cons from ecb_bsi where year=(select max(year) from ecb_bsi) group by geo) where mort is not null and cons is not null)
      select * from se union all select * from no union all select * from eu order by consumer_pct desc""")

    dump("analysis_se_mortgage_ts", """
      with stock as (select year, round(sum(value)/1000.0,0) housing_stock_sekbn from scb_household_loans_by_collateral
         where counterparty_sector='households' and monetary_financial_institutions_mfi='MFI' and month=12
         and collateral in ('single-family dwellings','condominiums','tenant-owner apartments','multi-dwelling building') group by year),
      rate as (select year, round(avg(value),2) mortgage_rate_pct from scb_lending_rates_by_purpose
         where purpose='1.2 Housing loans' and counterparty_sector='1 Households' and reference_sector='MFI' and agreement like 'new%' group by year)
      select s.year, s.housing_stock_sekbn, r.mortgage_rate_pct from stock s left join rate r using(year) where s.year>=2006 order by year""")

    dump("analysis_spending", """
      select geo, case coicop when 'CP01' then 'Food' when 'CP04' then 'Housing & utilities'
        when 'CP07' then 'Transport' when 'CP09' then 'Recreation & culture'
        when 'CP11' then 'Restaurants & hotels' when 'CP06' then 'Health' end as category,
        round(value/10.0,1) as pct_of_spending
      from hbs_str_t211 where year=2015 and unit='PM' and coicop in ('CP01','CP04','CP06','CP07','CP09','CP11')
        and geo in ('SE','NO','DK','FI','DE','FR','NL','IT','ES','PL','AT','BE')
      order by category, pct_of_spending desc""")

    dump("analysis_se_spending", """
      select lower(type_of_expenditure) as category, round(value,1) as pct_of_spending
      from scb_household_expenditure_by_type
      where type_of_household='all households' and measure='Share'
        and type_of_expenditure = upper(type_of_expenditure) and type_of_expenditure<>'TOTAL EXPENDITURE'
      order by pct_of_spending desc limit 10""")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
