//! TPC-H benchmark module for OpenSnow.
//!
//! Provides deterministic TPC-H data generation and query benchmarking
//! using Arrow arrays written as Parquet files.

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use arrow::array::{Float64Array, Int32Array, Int64Array, RecordBatch, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use parquet::arrow::ArrowWriter;
use parquet::basic::Compression;
use parquet::file::properties::WriterProperties;
use tracing::info;

use crate::engine::OpenSnowEngine;
use crate::error::Result;

// ---------------------------------------------------------------------------
// TPC-H SQL queries
// ---------------------------------------------------------------------------

/// Q1 - Pricing Summary Report
const TPCH_Q1: &str = r#"
SELECT
    l_returnflag,
    l_linestatus,
    SUM(l_quantity) AS sum_qty,
    SUM(l_extendedprice) AS sum_base_price,
    SUM(l_extendedprice * (1.0 - l_discount)) AS sum_disc_price,
    SUM(l_extendedprice * (1.0 - l_discount) * (1.0 + l_tax)) AS sum_charge,
    AVG(l_quantity) AS avg_qty,
    AVG(l_extendedprice) AS avg_price,
    AVG(l_discount) AS avg_disc,
    COUNT(*) AS count_order
FROM lineitem
WHERE l_shipdate <= '1998-09-02'
GROUP BY l_returnflag, l_linestatus
ORDER BY l_returnflag, l_linestatus
"#;

/// Q2 - Minimum Cost Supplier
const TPCH_Q2: &str = r#"
SELECT
    s_acctbal, s_name, n_name, p_partkey, p_mfgr,
    s_address, s_phone, s_comment
FROM part, supplier, partsupp, nation, region
WHERE p_partkey = ps_partkey
  AND s_suppkey = ps_suppkey
  AND p_size = 15
  AND p_type LIKE '%BRASS'
  AND s_nationkey = n_nationkey
  AND n_regionkey = r_regionkey
  AND r_name = 'EUROPE'
  AND ps_supplycost = (
      SELECT MIN(ps_supplycost)
      FROM partsupp, supplier, nation, region
      WHERE p_partkey = ps_partkey
        AND s_suppkey = ps_suppkey
        AND s_nationkey = n_nationkey
        AND n_regionkey = r_regionkey
        AND r_name = 'EUROPE'
  )
ORDER BY s_acctbal DESC, n_name, s_name, p_partkey
LIMIT 100
"#;

/// Q3 - Shipping Priority
const TPCH_Q3: &str = r#"
SELECT
    l_orderkey,
    SUM(l_extendedprice * (1.0 - l_discount)) AS revenue,
    o_orderdate,
    o_shippriority
FROM customer, orders, lineitem
WHERE c_mktsegment = 'BUILDING'
  AND c_custkey = o_custkey
  AND l_orderkey = o_orderkey
  AND o_orderdate < '1995-03-15'
  AND l_shipdate > '1995-03-15'
GROUP BY l_orderkey, o_orderdate, o_shippriority
ORDER BY revenue DESC, o_orderdate
LIMIT 10
"#;

/// Q4 - Order Priority Checking
const TPCH_Q4: &str = r#"
SELECT
    o_orderpriority,
    COUNT(*) AS order_count
FROM orders
WHERE o_orderdate >= '1993-07-01'
  AND o_orderdate < '1993-10-01'
  AND EXISTS (
      SELECT * FROM lineitem
      WHERE l_orderkey = o_orderkey
        AND l_commitdate < l_receiptdate
  )
GROUP BY o_orderpriority
ORDER BY o_orderpriority
"#;

/// Q5 - Local Supplier Volume
const TPCH_Q5: &str = r#"
SELECT
    n_name,
    SUM(l_extendedprice * (1.0 - l_discount)) AS revenue
FROM customer, orders, lineitem, supplier, nation, region
WHERE c_custkey = o_custkey
  AND l_orderkey = o_orderkey
  AND l_suppkey = s_suppkey
  AND c_nationkey = s_nationkey
  AND s_nationkey = n_nationkey
  AND n_regionkey = r_regionkey
  AND r_name = 'ASIA'
  AND o_orderdate >= '1994-01-01'
  AND o_orderdate < '1995-01-01'
GROUP BY n_name
ORDER BY revenue DESC
"#;

/// Q6 - Forecasting Revenue Change
const TPCH_Q6: &str = r#"
SELECT
    SUM(l_extendedprice * l_discount) AS revenue
FROM lineitem
WHERE l_shipdate >= '1994-01-01'
  AND l_shipdate < '1995-01-01'
  AND l_discount BETWEEN 0.05 AND 0.07
  AND l_quantity < 24.0
"#;

/// Q7 - Volume Shipping
const TPCH_Q7: &str = r#"
SELECT
    supp_nation, cust_nation, l_year, SUM(volume) AS revenue
FROM (
    SELECT
        n1.n_name AS supp_nation,
        n2.n_name AS cust_nation,
        EXTRACT(YEAR FROM l_shipdate) AS l_year,
        l_extendedprice * (1.0 - l_discount) AS volume
    FROM supplier, lineitem, orders, customer, nation n1, nation n2
    WHERE s_suppkey = l_suppkey
      AND o_orderkey = l_orderkey
      AND c_custkey = o_custkey
      AND s_nationkey = n1.n_nationkey
      AND c_nationkey = n2.n_nationkey
      AND (
          (n1.n_name = 'FRANCE' AND n2.n_name = 'GERMANY')
          OR (n1.n_name = 'GERMANY' AND n2.n_name = 'FRANCE')
      )
      AND l_shipdate BETWEEN '1995-01-01' AND '1996-12-31'
) AS shipping
GROUP BY supp_nation, cust_nation, l_year
ORDER BY supp_nation, cust_nation, l_year
"#;

/// Q8 - National Market Share
const TPCH_Q8: &str = r#"
SELECT
    o_year,
    SUM(CASE WHEN nation = 'BRAZIL' THEN volume ELSE 0.0 END) / SUM(volume) AS mkt_share
FROM (
    SELECT
        EXTRACT(YEAR FROM o_orderdate) AS o_year,
        l_extendedprice * (1.0 - l_discount) AS volume,
        n2.n_name AS nation
    FROM part, supplier, lineitem, orders, customer, nation n1, nation n2, region
    WHERE p_partkey = l_partkey
      AND s_suppkey = l_suppkey
      AND l_orderkey = o_orderkey
      AND o_custkey = c_custkey
      AND c_nationkey = n1.n_nationkey
      AND n1.n_regionkey = r_regionkey
      AND r_name = 'AMERICA'
      AND s_nationkey = n2.n_nationkey
      AND o_orderdate BETWEEN '1995-01-01' AND '1996-12-31'
      AND p_type = 'ECONOMY ANODIZED STEEL'
) AS all_nations
GROUP BY o_year
ORDER BY o_year
"#;

/// Q9 - Product Type Profit Measure
const TPCH_Q9: &str = r#"
SELECT
    nation, o_year, SUM(amount) AS sum_profit
FROM (
    SELECT
        n_name AS nation,
        EXTRACT(YEAR FROM o_orderdate) AS o_year,
        l_extendedprice * (1.0 - l_discount) - ps_supplycost * l_quantity AS amount
    FROM part, supplier, lineitem, partsupp, orders, nation
    WHERE s_suppkey = l_suppkey
      AND ps_suppkey = l_suppkey
      AND ps_partkey = l_partkey
      AND p_partkey = l_partkey
      AND o_orderkey = l_orderkey
      AND s_nationkey = n_nationkey
      AND p_name LIKE '%green%'
) AS profit
GROUP BY nation, o_year
ORDER BY nation, o_year DESC
"#;

/// Q10 - Returned Item Reporting
const TPCH_Q10: &str = r#"
SELECT
    c_custkey, c_name,
    SUM(l_extendedprice * (1.0 - l_discount)) AS revenue,
    c_acctbal, n_name, c_address, c_phone, c_comment
FROM customer, orders, lineitem, nation
WHERE c_custkey = o_custkey
  AND l_orderkey = o_orderkey
  AND o_orderdate >= '1993-10-01'
  AND o_orderdate < '1994-01-01'
  AND l_returnflag = 'R'
  AND c_nationkey = n_nationkey
GROUP BY c_custkey, c_name, c_acctbal, c_phone, n_name, c_address, c_comment
ORDER BY revenue DESC
LIMIT 20
"#;

/// Q11 - Important Stock Identification
const TPCH_Q11: &str = r#"
SELECT ps_partkey, SUM(ps_supplycost * ps_availqty) AS value
FROM partsupp, supplier, nation
WHERE ps_suppkey = s_suppkey
  AND s_nationkey = n_nationkey
  AND n_name = 'GERMANY'
GROUP BY ps_partkey
HAVING SUM(ps_supplycost * ps_availqty) > (
    SELECT SUM(ps_supplycost * ps_availqty) * 0.0001
    FROM partsupp, supplier, nation
    WHERE ps_suppkey = s_suppkey
      AND s_nationkey = n_nationkey
      AND n_name = 'GERMANY'
)
ORDER BY value DESC
"#;

/// Q12 - Shipping Modes and Order Priority
const TPCH_Q12: &str = r#"
SELECT
    l_shipmode,
    SUM(CASE
        WHEN o_orderpriority = '1-URGENT' OR o_orderpriority = '2-HIGH'
        THEN 1 ELSE 0
    END) AS high_line_count,
    SUM(CASE
        WHEN o_orderpriority <> '1-URGENT' AND o_orderpriority <> '2-HIGH'
        THEN 1 ELSE 0
    END) AS low_line_count
FROM orders, lineitem
WHERE o_orderkey = l_orderkey
  AND l_shipmode IN ('MAIL', 'SHIP')
  AND l_commitdate < l_receiptdate
  AND l_shipdate < l_commitdate
  AND l_receiptdate >= '1994-01-01'
  AND l_receiptdate < '1995-01-01'
GROUP BY l_shipmode
ORDER BY l_shipmode
"#;

/// Q13 - Customer Distribution
const TPCH_Q13: &str = r#"
SELECT c_count, COUNT(*) AS custdist
FROM (
    SELECT c_custkey, COUNT(o_orderkey) AS c_count
    FROM customer LEFT OUTER JOIN orders ON c_custkey = o_custkey
        AND o_comment NOT LIKE '%special%requests%'
    GROUP BY c_custkey
) AS c_orders
GROUP BY c_count
ORDER BY custdist DESC, c_count DESC
"#;

/// Q14 - Promotion Effect
const TPCH_Q14: &str = r#"
SELECT
    100.00 * SUM(CASE WHEN p_type LIKE 'PROMO%' THEN l_extendedprice * (1.0 - l_discount) ELSE 0.0 END)
        / SUM(l_extendedprice * (1.0 - l_discount)) AS promo_revenue
FROM lineitem, part
WHERE l_partkey = p_partkey
  AND l_shipdate >= '1995-09-01'
  AND l_shipdate < '1995-10-01'
"#;

/// Q15 - Top Supplier (simplified without view)
const TPCH_Q15: &str = r#"
WITH revenue AS (
    SELECT
        l_suppkey AS supplier_no,
        SUM(l_extendedprice * (1.0 - l_discount)) AS total_revenue
    FROM lineitem
    WHERE l_shipdate >= '1996-01-01'
      AND l_shipdate < '1996-04-01'
    GROUP BY l_suppkey
)
SELECT s_suppkey, s_name, s_address, s_phone, total_revenue
FROM supplier, revenue
WHERE s_suppkey = supplier_no
  AND total_revenue = (SELECT MAX(total_revenue) FROM revenue)
ORDER BY s_suppkey
"#;

/// Q16 - Parts/Supplier Relationship
const TPCH_Q16: &str = r#"
SELECT p_brand, p_type, p_size, COUNT(DISTINCT ps_suppkey) AS supplier_cnt
FROM partsupp, part
WHERE p_partkey = ps_partkey
  AND p_brand <> 'Brand#45'
  AND p_type NOT LIKE 'MEDIUM POLISHED%'
  AND p_size IN (49, 14, 23, 45, 19, 3, 36, 9)
  AND ps_suppkey NOT IN (
      SELECT s_suppkey FROM supplier WHERE s_comment LIKE '%Customer%Complaints%'
  )
GROUP BY p_brand, p_type, p_size
ORDER BY supplier_cnt DESC, p_brand, p_type, p_size
"#;

/// Q17 - Small-Quantity-Order Revenue
const TPCH_Q17: &str = r#"
SELECT SUM(l_extendedprice) / 7.0 AS avg_yearly
FROM lineitem, part
WHERE p_partkey = l_partkey
  AND p_brand = 'Brand#23'
  AND p_container = 'MED BOX'
  AND l_quantity < (
      SELECT 0.2 * AVG(l_quantity) FROM lineitem WHERE l_partkey = p_partkey
  )
"#;

/// Q18 - Large Volume Customer
const TPCH_Q18: &str = r#"
SELECT c_name, c_custkey, o_orderkey, o_orderdate, o_totalprice, SUM(l_quantity)
FROM customer, orders, lineitem
WHERE o_orderkey IN (
    SELECT l_orderkey FROM lineitem GROUP BY l_orderkey HAVING SUM(l_quantity) > 300.0
)
  AND c_custkey = o_custkey
  AND o_orderkey = l_orderkey
GROUP BY c_name, c_custkey, o_orderkey, o_orderdate, o_totalprice
ORDER BY o_totalprice DESC, o_orderdate
LIMIT 100
"#;

/// Q19 - Discounted Revenue
const TPCH_Q19: &str = r#"
SELECT SUM(l_extendedprice * (1.0 - l_discount)) AS revenue
FROM lineitem, part
WHERE (
    p_partkey = l_partkey
    AND p_brand = 'Brand#12'
    AND p_container IN ('SM CASE', 'SM BOX', 'SM PACK', 'SM PKG')
    AND l_quantity >= 1.0 AND l_quantity <= 11.0
    AND p_size BETWEEN 1 AND 5
    AND l_shipmode IN ('AIR', 'AIR REG')
    AND l_shipinstruct = 'DELIVER IN PERSON'
) OR (
    p_partkey = l_partkey
    AND p_brand = 'Brand#23'
    AND p_container IN ('MED BAG', 'MED BOX', 'MED PKG', 'MED PACK')
    AND l_quantity >= 10.0 AND l_quantity <= 20.0
    AND p_size BETWEEN 1 AND 10
    AND l_shipmode IN ('AIR', 'AIR REG')
    AND l_shipinstruct = 'DELIVER IN PERSON'
) OR (
    p_partkey = l_partkey
    AND p_brand = 'Brand#34'
    AND p_container IN ('LG CASE', 'LG BOX', 'LG PACK', 'LG PKG')
    AND l_quantity >= 20.0 AND l_quantity <= 30.0
    AND p_size BETWEEN 1 AND 15
    AND l_shipmode IN ('AIR', 'AIR REG')
    AND l_shipinstruct = 'DELIVER IN PERSON'
)
"#;

/// Q20 - Potential Part Promotion
const TPCH_Q20: &str = r#"
SELECT s_name, s_address
FROM supplier, nation
WHERE s_suppkey IN (
    SELECT ps_suppkey FROM partsupp
    WHERE ps_partkey IN (SELECT p_partkey FROM part WHERE p_name LIKE 'forest%')
      AND ps_availqty > (
          SELECT 0.5 * SUM(l_quantity) FROM lineitem
          WHERE l_partkey = ps_partkey AND l_suppkey = ps_suppkey
            AND l_shipdate >= '1994-01-01' AND l_shipdate < '1995-01-01'
      )
)
  AND s_nationkey = n_nationkey
  AND n_name = 'CANADA'
ORDER BY s_name
"#;

/// Q21 - Suppliers Who Kept Orders Waiting
const TPCH_Q21: &str = r#"
SELECT s_name, COUNT(*) AS numwait
FROM supplier, lineitem l1, orders, nation
WHERE s_suppkey = l1.l_suppkey
  AND o_orderkey = l1.l_orderkey
  AND o_orderstatus = 'F'
  AND l1.l_receiptdate > l1.l_commitdate
  AND EXISTS (
      SELECT * FROM lineitem l2
      WHERE l2.l_orderkey = l1.l_orderkey AND l2.l_suppkey <> l1.l_suppkey
  )
  AND NOT EXISTS (
      SELECT * FROM lineitem l3
      WHERE l3.l_orderkey = l1.l_orderkey AND l3.l_suppkey <> l1.l_suppkey
        AND l3.l_receiptdate > l3.l_commitdate
  )
  AND s_nationkey = n_nationkey
  AND n_name = 'SAUDI ARABIA'
GROUP BY s_name
ORDER BY numwait DESC, s_name
LIMIT 100
"#;

/// Q22 - Global Sales Opportunity
const TPCH_Q22: &str = r#"
SELECT cntrycode, COUNT(*) AS numcust, SUM(c_acctbal) AS totacctbal
FROM (
    SELECT SUBSTRING(c_phone FROM 1 FOR 2) AS cntrycode, c_acctbal
    FROM customer
    WHERE SUBSTRING(c_phone FROM 1 FOR 2) IN ('13','31','23','29','30','18','17')
      AND c_acctbal > (
          SELECT AVG(c_acctbal) FROM customer
          WHERE c_acctbal > 0.00
            AND SUBSTRING(c_phone FROM 1 FOR 2) IN ('13','31','23','29','30','18','17')
      )
      AND NOT EXISTS (SELECT * FROM orders WHERE o_custkey = c_custkey)
) AS custsale
GROUP BY cntrycode
ORDER BY cntrycode
"#;

/// All 22 TPC-H queries indexed by query number (1-based).
const TPCH_QUERIES: [&str; 22] = [
    TPCH_Q1, TPCH_Q2, TPCH_Q3, TPCH_Q4, TPCH_Q5, TPCH_Q6, TPCH_Q7, TPCH_Q8, TPCH_Q9, TPCH_Q10,
    TPCH_Q11, TPCH_Q12, TPCH_Q13, TPCH_Q14, TPCH_Q15, TPCH_Q16, TPCH_Q17, TPCH_Q18, TPCH_Q19,
    TPCH_Q20, TPCH_Q21, TPCH_Q22,
];

// ---------------------------------------------------------------------------
// Deterministic data generation helpers
// ---------------------------------------------------------------------------

/// Simple deterministic pseudo-random number generator (LCG).
struct Rng {
    state: u64,
}

impl Rng {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        // LCG parameters from Numerical Recipes
        self.state = self
            .state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.state
    }

    #[allow(dead_code)]
    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / ((1u64 << 53) as f64)
    }

    fn next_range(&mut self, lo: i64, hi: i64) -> i64 {
        let range = (hi - lo + 1) as u64;
        lo + (self.next_u64() % range) as i64
    }

    fn pick<'a>(&mut self, choices: &'a [&str]) -> &'a str {
        let idx = (self.next_u64() as usize) % choices.len();
        choices[idx]
    }

    fn gen_string(&mut self, min_len: usize, max_len: usize) -> String {
        let len = min_len + (self.next_u64() as usize) % (max_len - min_len + 1);
        (0..len)
            .map(|_| {
                let c = b'a' + (self.next_u64() % 26) as u8;
                c as char
            })
            .collect()
    }

    fn gen_phone(&mut self) -> String {
        let country = self.next_range(10, 34);
        format!(
            "{}-{:03}-{:03}-{:04}",
            country,
            self.next_range(100, 999),
            self.next_range(100, 999),
            self.next_range(1000, 9999)
        )
    }

    fn gen_date(&mut self, start_year: i32, end_year: i32) -> String {
        let year = self.next_range(start_year as i64, end_year as i64);
        let month = self.next_range(1, 12);
        let day = self.next_range(1, 28);
        format!("{:04}-{:02}-{:02}", year, month, day)
    }
}

// ---------------------------------------------------------------------------
// TPC-H reference data
// ---------------------------------------------------------------------------

const NATIONS: [(&str, i32); 25] = [
    ("ALGERIA", 0),
    ("ARGENTINA", 1),
    ("BRAZIL", 1),
    ("CANADA", 1),
    ("EGYPT", 4),
    ("ETHIOPIA", 0),
    ("FRANCE", 3),
    ("GERMANY", 3),
    ("INDIA", 2),
    ("INDONESIA", 2),
    ("IRAN", 4),
    ("IRAQ", 4),
    ("JAPAN", 2),
    ("JORDAN", 4),
    ("KENYA", 0),
    ("MOROCCO", 0),
    ("MOZAMBIQUE", 0),
    ("PERU", 1),
    ("CHINA", 2),
    ("ROMANIA", 3),
    ("SAUDI ARABIA", 4),
    ("VIETNAM", 2),
    ("RUSSIA", 3),
    ("UNITED KINGDOM", 3),
    ("UNITED STATES", 1),
];

const REGIONS: [&str; 5] = ["AFRICA", "AMERICA", "ASIA", "EUROPE", "MIDDLE EAST"];

const SEGMENTS: [&str; 5] = [
    "AUTOMOBILE",
    "BUILDING",
    "FURNITURE",
    "HOUSEHOLD",
    "MACHINERY",
];

const ORDER_PRIORITIES: [&str; 5] = ["1-URGENT", "2-HIGH", "3-MEDIUM", "4-NOT SPECIFIED", "5-LOW"];

const ORDER_STATUS: [&str; 3] = ["F", "O", "P"];

const SHIP_MODES: [&str; 7] = ["REG AIR", "AIR", "RAIL", "SHIP", "TRUCK", "MAIL", "FOB"];

const SHIP_INSTRUCTS: [&str; 4] = [
    "DELIVER IN PERSON",
    "COLLECT COD",
    "NONE",
    "TAKE BACK RETURN",
];

const RETURN_FLAGS: [&str; 3] = ["A", "N", "R"];

const LINE_STATUS: [&str; 2] = ["F", "O"];

const PART_TYPES: [&str; 5] = [
    "ECONOMY ANODIZED STEEL",
    "STANDARD POLISHED TIN",
    "PROMO BURNISHED COPPER",
    "MEDIUM POLISHED BRASS",
    "SMALL PLATED NICKEL",
];

const PART_CONTAINERS: [&str; 8] = [
    "SM CASE", "SM BOX", "SM PACK", "SM PKG", "MED BAG", "MED BOX", "MED PKG", "MED PACK",
];

const BRANDS: [&str; 5] = ["Brand#12", "Brand#23", "Brand#34", "Brand#45", "Brand#51"];

// ---------------------------------------------------------------------------
// Data generation
// ---------------------------------------------------------------------------

fn write_parquet(path: &Path, batch: &RecordBatch) -> Result<()> {
    let file = fs::File::create(path)?;
    let props = WriterProperties::builder()
        .set_compression(Compression::SNAPPY)
        .build();
    let mut writer = ArrowWriter::try_new(file, batch.schema(), Some(props))?;
    writer.write(batch)?;
    writer.close()?;
    Ok(())
}

fn gen_region(dir: &Path) -> Result<()> {
    let mut keys = Vec::with_capacity(5);
    let mut names = Vec::with_capacity(5);
    let mut comments = Vec::with_capacity(5);
    let mut rng = Rng::new(100);
    for (i, region) in REGIONS.iter().enumerate() {
        keys.push(i as i32);
        names.push(region.to_string());
        comments.push(rng.gen_string(30, 80));
    }
    let schema = Arc::new(Schema::new(vec![
        Field::new("r_regionkey", DataType::Int32, false),
        Field::new("r_name", DataType::Utf8, false),
        Field::new("r_comment", DataType::Utf8, true),
    ]));
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(Int32Array::from(keys)),
            Arc::new(StringArray::from(names)),
            Arc::new(StringArray::from(comments)),
        ],
    )?;
    write_parquet(&dir.join("region.parquet"), &batch)
}

fn gen_nation(dir: &Path) -> Result<()> {
    let mut keys = Vec::with_capacity(25);
    let mut names = Vec::with_capacity(25);
    let mut region_keys = Vec::with_capacity(25);
    let mut comments = Vec::with_capacity(25);
    let mut rng = Rng::new(200);
    for (i, &(name, rkey)) in NATIONS.iter().enumerate() {
        keys.push(i as i32);
        names.push(name.to_string());
        region_keys.push(rkey);
        comments.push(rng.gen_string(30, 80));
    }
    let schema = Arc::new(Schema::new(vec![
        Field::new("n_nationkey", DataType::Int32, false),
        Field::new("n_name", DataType::Utf8, false),
        Field::new("n_regionkey", DataType::Int32, false),
        Field::new("n_comment", DataType::Utf8, true),
    ]));
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(Int32Array::from(keys)),
            Arc::new(StringArray::from(names)),
            Arc::new(Int32Array::from(region_keys)),
            Arc::new(StringArray::from(comments)),
        ],
    )?;
    write_parquet(&dir.join("nation.parquet"), &batch)
}

fn gen_supplier(dir: &Path, sf: f64) -> Result<()> {
    let count = (100.0 * sf).max(10.0) as usize;
    let mut rng = Rng::new(300);
    let mut suppkeys = Vec::with_capacity(count);
    let mut names = Vec::with_capacity(count);
    let mut addresses = Vec::with_capacity(count);
    let mut nationkeys = Vec::with_capacity(count);
    let mut phones = Vec::with_capacity(count);
    let mut acctbals = Vec::with_capacity(count);
    let mut comments = Vec::with_capacity(count);

    for i in 1..=count {
        suppkeys.push(i as i64);
        names.push(format!("Supplier#{:09}", i));
        addresses.push(rng.gen_string(10, 40));
        nationkeys.push(rng.next_range(0, 24) as i32);
        phones.push(rng.gen_phone());
        acctbals.push((rng.next_range(-99999, 999999) as f64) / 100.0);
        // Ensure some suppliers have "Customer" + "Complaints" for Q16
        if i % 50 == 0 {
            comments.push(format!(
                "{}Customer{}Complaints{}",
                rng.gen_string(5, 10),
                rng.gen_string(1, 5),
                rng.gen_string(5, 10)
            ));
        } else {
            comments.push(rng.gen_string(30, 80));
        }
    }

    let schema = Arc::new(Schema::new(vec![
        Field::new("s_suppkey", DataType::Int64, false),
        Field::new("s_name", DataType::Utf8, false),
        Field::new("s_address", DataType::Utf8, false),
        Field::new("s_nationkey", DataType::Int32, false),
        Field::new("s_phone", DataType::Utf8, false),
        Field::new("s_acctbal", DataType::Float64, false),
        Field::new("s_comment", DataType::Utf8, true),
    ]));
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(Int64Array::from(suppkeys)),
            Arc::new(StringArray::from(names)),
            Arc::new(StringArray::from(addresses)),
            Arc::new(Int32Array::from(nationkeys)),
            Arc::new(StringArray::from(phones)),
            Arc::new(Float64Array::from(acctbals)),
            Arc::new(StringArray::from(comments)),
        ],
    )?;
    write_parquet(&dir.join("supplier.parquet"), &batch)
}

fn gen_customer(dir: &Path, sf: f64) -> Result<()> {
    // SF 0.01 -> 1500, SF 1.0 -> 150000
    let count = (150_000.0 * sf).max(100.0) as usize;
    let mut rng = Rng::new(400);
    let mut custkeys = Vec::with_capacity(count);
    let mut names = Vec::with_capacity(count);
    let mut addresses = Vec::with_capacity(count);
    let mut nationkeys = Vec::with_capacity(count);
    let mut phones = Vec::with_capacity(count);
    let mut acctbals = Vec::with_capacity(count);
    let mut mktsegs = Vec::with_capacity(count);
    let mut comments = Vec::with_capacity(count);

    for i in 1..=count {
        custkeys.push(i as i64);
        names.push(format!("Customer#{:09}", i));
        addresses.push(rng.gen_string(10, 40));
        nationkeys.push(rng.next_range(0, 24) as i32);
        phones.push(rng.gen_phone());
        acctbals.push((rng.next_range(-99999, 999999) as f64) / 100.0);
        mktsegs.push(rng.pick(&SEGMENTS).to_string());
        // Some comments with "special" + "requests" for Q13
        if i % 100 == 0 {
            comments.push(format!(
                "{}special{}requests{}",
                rng.gen_string(5, 10),
                rng.gen_string(1, 3),
                rng.gen_string(5, 10)
            ));
        } else {
            comments.push(rng.gen_string(30, 80));
        }
    }

    let schema = Arc::new(Schema::new(vec![
        Field::new("c_custkey", DataType::Int64, false),
        Field::new("c_name", DataType::Utf8, false),
        Field::new("c_address", DataType::Utf8, false),
        Field::new("c_nationkey", DataType::Int32, false),
        Field::new("c_phone", DataType::Utf8, false),
        Field::new("c_acctbal", DataType::Float64, false),
        Field::new("c_mktsegment", DataType::Utf8, false),
        Field::new("c_comment", DataType::Utf8, true),
    ]));
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(Int64Array::from(custkeys)),
            Arc::new(StringArray::from(names)),
            Arc::new(StringArray::from(addresses)),
            Arc::new(Int32Array::from(nationkeys)),
            Arc::new(StringArray::from(phones)),
            Arc::new(Float64Array::from(acctbals)),
            Arc::new(StringArray::from(mktsegs)),
            Arc::new(StringArray::from(comments)),
        ],
    )?;
    write_parquet(&dir.join("customer.parquet"), &batch)
}

fn gen_part(dir: &Path, sf: f64) -> Result<()> {
    let count = (200_000.0 * sf).max(100.0) as usize;
    let mut rng = Rng::new(500);
    let mut partkeys = Vec::with_capacity(count);
    let mut names = Vec::with_capacity(count);
    let mut mfgrs = Vec::with_capacity(count);
    let mut brands = Vec::with_capacity(count);
    let mut types = Vec::with_capacity(count);
    let mut sizes = Vec::with_capacity(count);
    let mut containers = Vec::with_capacity(count);
    let mut retail_prices = Vec::with_capacity(count);
    let mut comments = Vec::with_capacity(count);

    let name_words = [
        "forest", "green", "almond", "snow", "steel", "copper", "tin", "brass", "nickel",
    ];

    for i in 1..=count {
        partkeys.push(i as i64);
        // Some names contain "green" or "forest" for Q9/Q20
        let word1 = rng.pick(&name_words);
        let word2 = rng.pick(&name_words);
        names.push(format!("{} {} part{}", word1, word2, i));
        mfgrs.push(format!("Manufacturer#{}", rng.next_range(1, 5)));
        brands.push(rng.pick(&BRANDS).to_string());
        types.push(rng.pick(&PART_TYPES).to_string());
        sizes.push(rng.next_range(1, 50) as i32);
        containers.push(rng.pick(&PART_CONTAINERS).to_string());
        retail_prices.push((rng.next_range(100, 200000) as f64) / 100.0);
        comments.push(rng.gen_string(10, 30));
    }

    let schema = Arc::new(Schema::new(vec![
        Field::new("p_partkey", DataType::Int64, false),
        Field::new("p_name", DataType::Utf8, false),
        Field::new("p_mfgr", DataType::Utf8, false),
        Field::new("p_brand", DataType::Utf8, false),
        Field::new("p_type", DataType::Utf8, false),
        Field::new("p_size", DataType::Int32, false),
        Field::new("p_container", DataType::Utf8, false),
        Field::new("p_retailprice", DataType::Float64, false),
        Field::new("p_comment", DataType::Utf8, true),
    ]));
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(Int64Array::from(partkeys)),
            Arc::new(StringArray::from(names)),
            Arc::new(StringArray::from(mfgrs)),
            Arc::new(StringArray::from(brands)),
            Arc::new(StringArray::from(types)),
            Arc::new(Int32Array::from(sizes)),
            Arc::new(StringArray::from(containers)),
            Arc::new(Float64Array::from(retail_prices)),
            Arc::new(StringArray::from(comments)),
        ],
    )?;
    write_parquet(&dir.join("part.parquet"), &batch)
}

fn gen_partsupp(dir: &Path, sf: f64) -> Result<()> {
    let part_count = (200_000.0 * sf).max(100.0) as usize;
    let supp_count = (100.0 * sf).max(10.0) as usize;
    // 4 entries per part in TPC-H
    let count = part_count * 4;
    let mut rng = Rng::new(600);
    let mut partkeys = Vec::with_capacity(count);
    let mut suppkeys = Vec::with_capacity(count);
    let mut availqtys = Vec::with_capacity(count);
    let mut supplycosts = Vec::with_capacity(count);
    let mut comments = Vec::with_capacity(count);

    for pk in 1..=part_count {
        for j in 0..4 {
            partkeys.push(pk as i64);
            let sk = ((pk + j * (supp_count / 4 + (pk - 1) / supp_count)) % supp_count) + 1;
            suppkeys.push(sk as i64);
            availqtys.push(rng.next_range(1, 9999) as i32);
            supplycosts.push((rng.next_range(100, 100000) as f64) / 100.0);
            comments.push(rng.gen_string(50, 150));
        }
    }

    let schema = Arc::new(Schema::new(vec![
        Field::new("ps_partkey", DataType::Int64, false),
        Field::new("ps_suppkey", DataType::Int64, false),
        Field::new("ps_availqty", DataType::Int32, false),
        Field::new("ps_supplycost", DataType::Float64, false),
        Field::new("ps_comment", DataType::Utf8, true),
    ]));
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(Int64Array::from(partkeys)),
            Arc::new(Int64Array::from(suppkeys)),
            Arc::new(Int32Array::from(availqtys)),
            Arc::new(Float64Array::from(supplycosts)),
            Arc::new(StringArray::from(comments)),
        ],
    )?;
    write_parquet(&dir.join("partsupp.parquet"), &batch)
}

fn gen_orders_and_lineitem(dir: &Path, sf: f64) -> Result<()> {
    let order_count = (1_500_000.0 * sf).max(1000.0) as usize;
    let cust_count = (150_000.0 * sf).max(100.0) as usize;
    let supp_count = (100.0 * sf).max(10.0) as usize;
    let part_count = (200_000.0 * sf).max(100.0) as usize;

    let mut rng = Rng::new(700);

    // Orders columns
    let mut o_orderkeys = Vec::with_capacity(order_count);
    let mut o_custkeys = Vec::with_capacity(order_count);
    let mut o_orderstatuses = Vec::with_capacity(order_count);
    let mut o_totalprices = Vec::with_capacity(order_count);
    let mut o_orderdates = Vec::with_capacity(order_count);
    let mut o_orderpriorities = Vec::with_capacity(order_count);
    let mut o_clerks = Vec::with_capacity(order_count);
    let mut o_shippriorities = Vec::with_capacity(order_count);
    let mut o_comments = Vec::with_capacity(order_count);

    // Lineitem columns
    let est_lines = order_count * 4; // ~4 lineitems per order on average
    let mut l_orderkeys = Vec::with_capacity(est_lines);
    let mut l_partkeys = Vec::with_capacity(est_lines);
    let mut l_suppkeys = Vec::with_capacity(est_lines);
    let mut l_linenumbers = Vec::with_capacity(est_lines);
    let mut l_quantities = Vec::with_capacity(est_lines);
    let mut l_extendedprices = Vec::with_capacity(est_lines);
    let mut l_discounts = Vec::with_capacity(est_lines);
    let mut l_taxes = Vec::with_capacity(est_lines);
    let mut l_returnflags = Vec::with_capacity(est_lines);
    let mut l_linestatuses = Vec::with_capacity(est_lines);
    let mut l_shipdates = Vec::with_capacity(est_lines);
    let mut l_commitdates = Vec::with_capacity(est_lines);
    let mut l_receiptdates = Vec::with_capacity(est_lines);
    let mut l_shipmodes = Vec::with_capacity(est_lines);
    let mut l_shipinstructs = Vec::with_capacity(est_lines);
    let mut l_comments = Vec::with_capacity(est_lines);

    for i in 1..=order_count {
        let orderkey = i as i64;
        let custkey = rng.next_range(1, cust_count as i64);
        let order_date = rng.gen_date(1992, 1998);
        let status = rng.pick(&ORDER_STATUS).to_string();
        let priority = rng.pick(&ORDER_PRIORITIES).to_string();
        let clerk = format!("Clerk#{:09}", rng.next_range(1, 1000));

        let num_items = rng.next_range(1, 7) as usize;
        let mut total_price = 0.0;

        for ln in 1..=num_items {
            let partkey = rng.next_range(1, part_count as i64);
            let suppkey = rng.next_range(1, supp_count as i64);
            let qty = rng.next_range(1, 50) as f64;
            let price = ((rng.next_range(100, 200000) as f64) / 100.0) * qty / 50.0;
            let discount = (rng.next_range(0, 10) as f64) / 100.0;
            let tax = (rng.next_range(0, 8) as f64) / 100.0;

            total_price += price * (1.0 - discount) * (1.0 + tax);

            l_orderkeys.push(orderkey);
            l_partkeys.push(partkey);
            l_suppkeys.push(suppkey);
            l_linenumbers.push(ln as i32);
            l_quantities.push(qty);
            l_extendedprices.push(price);
            l_discounts.push(discount);
            l_taxes.push(tax);
            l_returnflags.push(rng.pick(&RETURN_FLAGS).to_string());
            l_linestatuses.push(rng.pick(&LINE_STATUS).to_string());
            l_shipdates.push(rng.gen_date(1992, 1998));
            l_commitdates.push(rng.gen_date(1992, 1998));
            l_receiptdates.push(rng.gen_date(1992, 1998));
            l_shipmodes.push(rng.pick(&SHIP_MODES).to_string());
            l_shipinstructs.push(rng.pick(&SHIP_INSTRUCTS).to_string());
            l_comments.push(rng.gen_string(10, 40));
        }

        o_orderkeys.push(orderkey);
        o_custkeys.push(custkey);
        o_orderstatuses.push(status);
        o_totalprices.push(total_price);
        o_orderdates.push(order_date);
        o_orderpriorities.push(priority);
        o_clerks.push(clerk);
        o_shippriorities.push(0i32);
        o_comments.push(rng.gen_string(20, 70));
    }

    // Write orders
    let orders_schema = Arc::new(Schema::new(vec![
        Field::new("o_orderkey", DataType::Int64, false),
        Field::new("o_custkey", DataType::Int64, false),
        Field::new("o_orderstatus", DataType::Utf8, false),
        Field::new("o_totalprice", DataType::Float64, false),
        Field::new("o_orderdate", DataType::Utf8, false),
        Field::new("o_orderpriority", DataType::Utf8, false),
        Field::new("o_clerk", DataType::Utf8, false),
        Field::new("o_shippriority", DataType::Int32, false),
        Field::new("o_comment", DataType::Utf8, true),
    ]));
    let orders_batch = RecordBatch::try_new(
        orders_schema,
        vec![
            Arc::new(Int64Array::from(o_orderkeys)),
            Arc::new(Int64Array::from(o_custkeys)),
            Arc::new(StringArray::from(o_orderstatuses)),
            Arc::new(Float64Array::from(o_totalprices)),
            Arc::new(StringArray::from(o_orderdates)),
            Arc::new(StringArray::from(o_orderpriorities)),
            Arc::new(StringArray::from(o_clerks)),
            Arc::new(Int32Array::from(o_shippriorities)),
            Arc::new(StringArray::from(o_comments)),
        ],
    )?;
    write_parquet(&dir.join("orders.parquet"), &orders_batch)?;

    // Write lineitem
    let lineitem_schema = Arc::new(Schema::new(vec![
        Field::new("l_orderkey", DataType::Int64, false),
        Field::new("l_partkey", DataType::Int64, false),
        Field::new("l_suppkey", DataType::Int64, false),
        Field::new("l_linenumber", DataType::Int32, false),
        Field::new("l_quantity", DataType::Float64, false),
        Field::new("l_extendedprice", DataType::Float64, false),
        Field::new("l_discount", DataType::Float64, false),
        Field::new("l_tax", DataType::Float64, false),
        Field::new("l_returnflag", DataType::Utf8, false),
        Field::new("l_linestatus", DataType::Utf8, false),
        Field::new("l_shipdate", DataType::Utf8, false),
        Field::new("l_commitdate", DataType::Utf8, false),
        Field::new("l_receiptdate", DataType::Utf8, false),
        Field::new("l_shipinstruct", DataType::Utf8, false),
        Field::new("l_shipmode", DataType::Utf8, false),
        Field::new("l_comment", DataType::Utf8, true),
    ]));
    let lineitem_batch = RecordBatch::try_new(
        lineitem_schema,
        vec![
            Arc::new(Int64Array::from(l_orderkeys)),
            Arc::new(Int64Array::from(l_partkeys)),
            Arc::new(Int64Array::from(l_suppkeys)),
            Arc::new(Int32Array::from(l_linenumbers)),
            Arc::new(Float64Array::from(l_quantities)),
            Arc::new(Float64Array::from(l_extendedprices)),
            Arc::new(Float64Array::from(l_discounts)),
            Arc::new(Float64Array::from(l_taxes)),
            Arc::new(StringArray::from(l_returnflags)),
            Arc::new(StringArray::from(l_linestatuses)),
            Arc::new(StringArray::from(l_shipdates)),
            Arc::new(StringArray::from(l_commitdates)),
            Arc::new(StringArray::from(l_receiptdates)),
            Arc::new(StringArray::from(l_shipinstructs)),
            Arc::new(StringArray::from(l_shipmodes)),
            Arc::new(StringArray::from(l_comments)),
        ],
    )?;
    write_parquet(&dir.join("lineitem.parquet"), &lineitem_batch)?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Generate all 8 TPC-H tables as Parquet files in the given directory.
///
/// Scale factor controls data size: 1.0 = ~1GB, 0.01 = ~10MB for quick tests.
/// For SF 0.01: lineitem ~60K rows, orders ~15K, customer ~1500, part ~2000,
/// supplier ~100, partsupp ~8000, nation ~25, region ~5.
pub fn generate_tpch_data(path: &str, scale_factor: f64) -> Result<()> {
    let dir = Path::new(path);
    fs::create_dir_all(dir)?;

    info!(
        "Generating TPC-H data at {} with scale factor {}",
        path, scale_factor
    );
    let start = Instant::now();

    gen_region(dir)?;
    info!("  region: 5 rows");

    gen_nation(dir)?;
    info!("  nation: 25 rows");

    let supp_count = (100.0 * scale_factor).max(10.0) as usize;
    gen_supplier(dir, scale_factor)?;
    info!("  supplier: {} rows", supp_count);

    let cust_count = (150_000.0 * scale_factor).max(100.0) as usize;
    gen_customer(dir, scale_factor)?;
    info!("  customer: {} rows", cust_count);

    let part_count = (200_000.0 * scale_factor).max(100.0) as usize;
    gen_part(dir, scale_factor)?;
    info!("  part: {} rows", part_count);

    let ps_count = part_count * 4;
    gen_partsupp(dir, scale_factor)?;
    info!("  partsupp: {} rows", ps_count);

    let order_count = (1_500_000.0 * scale_factor).max(1000.0) as usize;
    gen_orders_and_lineitem(dir, scale_factor)?;
    info!("  orders: ~{} rows", order_count);
    info!("  lineitem: ~{} rows (avg 4 per order)", order_count * 4);

    let elapsed = start.elapsed();
    info!(
        "TPC-H data generation completed in {:.2}s",
        elapsed.as_secs_f64()
    );
    println!(
        "Generated TPC-H data (SF={}) at {} in {:.2}s",
        scale_factor,
        path,
        elapsed.as_secs_f64()
    );

    Ok(())
}

/// Register all 8 TPC-H tables from Parquet files in the given directory
/// with the engine's session context.
async fn register_tpch_tables(engine: &OpenSnowEngine, data_path: &str) -> Result<()> {
    let tables = [
        "region", "nation", "supplier", "customer", "part", "partsupp", "orders", "lineitem",
    ];
    for table in &tables {
        let path = format!("{}/{}.parquet", data_path, table);
        engine.register_parquet(table, &path).await?;
    }
    Ok(())
}

/// Result of a single TPC-H query execution.
#[derive(Debug, Clone)]
pub struct QueryResult {
    /// TPC-H query number (1-22).
    pub query_number: usize,
    /// Wall-clock execution time.
    pub elapsed: std::time::Duration,
    /// Number of result rows.
    pub row_count: usize,
    /// Whether the query succeeded.
    pub success: bool,
    /// Error message if the query failed.
    pub error: Option<String>,
}

/// Run TPC-H benchmark queries against the engine.
///
/// The `data_path` directory must contain the Parquet files generated by
/// [`generate_tpch_data`]. If `queries` is empty, all 22 queries are run.
///
/// Returns a vector of [`QueryResult`] for each query executed.
pub async fn run_tpch_benchmark(
    engine: &OpenSnowEngine,
    data_path: &str,
    queries: &[usize],
) -> Result<Vec<QueryResult>> {
    // Register tables
    register_tpch_tables(engine, data_path).await?;

    let query_nums: Vec<usize> = if queries.is_empty() {
        (1..=22).collect()
    } else {
        queries.to_vec()
    };

    println!();
    println!("{:=<70}", "");
    println!(" OpenSnow TPC-H Benchmark");
    println!("{:=<70}", "");
    println!(
        "{:<8} {:>12} {:>10} {:>8}",
        "Query", "Time (ms)", "Rows", "Status"
    );
    println!("{:-<70}", "");

    let mut results = Vec::with_capacity(query_nums.len());
    let total_start = Instant::now();

    for &qnum in &query_nums {
        if !(1..=22).contains(&qnum) {
            eprintln!("Skipping invalid query number: {}", qnum);
            continue;
        }

        let sql = TPCH_QUERIES[qnum - 1];
        let start = Instant::now();

        match engine.execute_sql_raw(sql).await {
            Ok(batches) => {
                let elapsed = start.elapsed();
                let row_count: usize = batches.iter().map(|b| b.num_rows()).sum();
                println!(
                    "Q{:<6} {:>10.2}ms {:>10} {:>8}",
                    qnum,
                    elapsed.as_secs_f64() * 1000.0,
                    row_count,
                    "OK"
                );
                results.push(QueryResult {
                    query_number: qnum,
                    elapsed,
                    row_count,
                    success: true,
                    error: None,
                });
            }
            Err(e) => {
                let elapsed = start.elapsed();
                println!(
                    "Q{:<6} {:>10.2}ms {:>10} {:>8}",
                    qnum,
                    elapsed.as_secs_f64() * 1000.0,
                    "-",
                    "FAIL"
                );
                info!("Q{} error: {}", qnum, e);
                results.push(QueryResult {
                    query_number: qnum,
                    elapsed,
                    row_count: 0,
                    success: false,
                    error: Some(format!("{}", e)),
                });
            }
        }
    }

    let total_elapsed = total_start.elapsed();
    println!("{:-<70}", "");

    let ok_count = results.iter().filter(|r| r.success).count();
    let fail_count = results.iter().filter(|r| !r.success).count();
    let total_query_ms: f64 = results
        .iter()
        .map(|r| r.elapsed.as_secs_f64() * 1000.0)
        .sum();

    println!(
        "Total: {:.2}ms | Passed: {} | Failed: {} | Wall: {:.2}s",
        total_query_ms,
        ok_count,
        fail_count,
        total_elapsed.as_secs_f64()
    );
    println!("{:=<70}", "");
    println!();

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rng_deterministic() {
        let mut r1 = Rng::new(42);
        let mut r2 = Rng::new(42);
        for _ in 0..100 {
            assert_eq!(r1.next_u64(), r2.next_u64());
        }
    }

    #[test]
    fn test_generate_tpch_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap();
        generate_tpch_data(path, 0.01).unwrap();

        // Check that all 8 files exist
        for table in &[
            "region", "nation", "supplier", "customer", "part", "partsupp", "orders", "lineitem",
        ] {
            let p = dir.path().join(format!("{}.parquet", table));
            assert!(p.exists(), "Missing table: {}", table);
        }
    }

    #[tokio::test]
    async fn test_run_benchmark_subset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap();
        generate_tpch_data(path, 0.01).unwrap();

        let engine = OpenSnowEngine::new();
        // Run a subset: Q1, Q6 (simpler queries that are most likely to succeed)
        let results = run_tpch_benchmark(&engine, path, &[1, 6]).await.unwrap();
        assert_eq!(results.len(), 2);
        // Q1 and Q6 are simple aggregations and should succeed
        for r in &results {
            assert!(r.success, "Q{} failed: {:?}", r.query_number, r.error);
        }
    }
}
