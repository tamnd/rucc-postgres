-- pricing summary
select l_returnflag, l_linestatus, sum(l_quantity) as qty, sum(l_extendedprice) as base,
    sum(l_extendedprice * (1 - l_discount)) as disc, sum(l_extendedprice * (1 - l_discount) * (1 + l_tax)) as charge,
    avg(l_quantity) as avg_qty, avg(l_extendedprice) as avg_price, avg(l_discount) as avg_disc, count(*)
from lineitem
where l_shipdate <= date '1998-08-01'
group by l_returnflag, l_linestatus
order by l_returnflag, l_linestatus;
-- cheapest supplier
select s_acctbal, s_name, n_name, p_partkey, p_brand
from part, supplier, partsupp, nation, region
where p_partkey = ps_partkey and s_suppkey = ps_suppkey and p_size = 15 and p_type like '%BRASS'
    and s_nationkey = n_nationkey and n_regionkey = r_regionkey and r_name = 'EUROPE'
    and ps_supplycost = (
        select min(ps_supplycost) from partsupp, supplier, nation, region
        where p_partkey = ps_partkey and s_suppkey = ps_suppkey and s_nationkey = n_nationkey
            and n_regionkey = r_regionkey and r_name = 'EUROPE')
order by s_acctbal desc, n_name, s_name, p_partkey
limit 100;
-- shipping priority
select l_orderkey, sum(l_extendedprice * (1 - l_discount)) as revenue, o_orderdate, o_shippriority
from customer, orders, lineitem
where c_mktsegment = 'BUILDING' and c_custkey = o_custkey and l_orderkey = o_orderkey
    and o_orderdate < date '1995-03-15' and l_shipdate > date '1995-03-15'
group by l_orderkey, o_orderdate, o_shippriority
order by revenue desc, o_orderdate, l_orderkey
limit 10;
-- order priority
select o_orderpriority, count(*)
from orders
where o_orderdate >= date '1993-07-01' and o_orderdate < date '1993-10-01'
    and exists (select from lineitem where l_orderkey = o_orderkey and l_commitdate < l_receiptdate)
group by o_orderpriority
order by o_orderpriority;
-- local supplier volume
select n_name, sum(l_extendedprice * (1 - l_discount)) as revenue
from customer, orders, lineitem, supplier, nation, region
where c_custkey = o_custkey and l_orderkey = o_orderkey and l_suppkey = s_suppkey
    and c_nationkey = s_nationkey and s_nationkey = n_nationkey and n_regionkey = r_regionkey
    and r_name = 'ASIA' and o_orderdate >= date '1994-01-01' and o_orderdate < date '1995-01-01'
group by n_name
order by revenue desc, n_name;
-- revenue change
select sum(l_extendedprice * l_discount) as revenue
from lineitem
where l_shipdate >= date '1994-01-01' and l_shipdate < date '1995-01-01'
    and l_discount between 0.05 and 0.07 and l_quantity < 24;
-- volume shipping
select supp_nation, cust_nation, year, sum(volume) as revenue
from (
    select n1.n_name as supp_nation, n2.n_name as cust_nation, extract(year from l_shipdate) as year,
        l_extendedprice * (1 - l_discount) as volume
    from supplier, lineitem, orders, customer, nation n1, nation n2
    where s_suppkey = l_suppkey and o_orderkey = l_orderkey and c_custkey = o_custkey
        and s_nationkey = n1.n_nationkey and c_nationkey = n2.n_nationkey
        and ((n1.n_name = 'NATION 6' and n2.n_name = 'NATION 7')
            or (n1.n_name = 'NATION 7' and n2.n_name = 'NATION 6'))
        and l_shipdate between date '1995-01-01' and date '1996-12-31') shipping
group by supp_nation, cust_nation, year
order by supp_nation, cust_nation, year;
-- market share
select year, sum(case when nation = 'NATION 8' then volume else 0 end) / sum(volume) as share
from (
    select extract(year from o_orderdate) as year, l_extendedprice * (1 - l_discount) as volume,
        n2.n_name as nation
    from part, supplier, lineitem, orders, customer, nation n1, nation n2, region
    where p_partkey = l_partkey and s_suppkey = l_suppkey and l_orderkey = o_orderkey
        and o_custkey = c_custkey and c_nationkey = n1.n_nationkey and n1.n_regionkey = r_regionkey
        and r_name = 'AMERICA' and s_nationkey = n2.n_nationkey
        and o_orderdate between date '1995-01-01' and date '1996-12-31'
        and p_type = 'ECONOMY ANODIZED STEEL') all_nations
group by year
order by year;
-- product type profit
select nation, year, sum(amount) as profit
from (
    select n_name as nation, extract(year from o_orderdate) as year,
        l_extendedprice * (1 - l_discount) - ps_supplycost * l_quantity as amount
    from part, supplier, lineitem, partsupp, orders, nation
    where s_suppkey = l_suppkey and ps_suppkey = l_suppkey and ps_partkey = l_partkey
        and p_partkey = l_partkey and o_orderkey = l_orderkey and s_nationkey = n_nationkey
        and p_type like '%BRASS') profit
group by nation, year
order by nation, year desc;
-- returned items
select c_custkey, c_name, sum(l_extendedprice * (1 - l_discount)) as revenue, c_acctbal, n_name
from customer, orders, lineitem, nation
where c_custkey = o_custkey and l_orderkey = o_orderkey and o_orderdate >= date '1993-10-01'
    and o_orderdate < date '1994-01-01' and l_returnflag = 'R' and c_nationkey = n_nationkey
group by c_custkey, c_name, c_acctbal, n_name
order by revenue desc, c_custkey
limit 20;
-- important stock
select ps_partkey, sum(ps_supplycost * ps_availqty) as value
from partsupp, supplier, nation
where ps_suppkey = s_suppkey and s_nationkey = n_nationkey and n_name = 'NATION 7'
group by ps_partkey
having sum(ps_supplycost * ps_availqty) > (
    select sum(ps_supplycost * ps_availqty) * 0.0001
    from partsupp, supplier, nation
    where ps_suppkey = s_suppkey and s_nationkey = n_nationkey and n_name = 'NATION 7')
order by value desc, ps_partkey;
-- shipping modes
select l_shipmode,
    sum(case when o_orderpriority in ('1-URGENT', '2-HIGH') then 1 else 0 end) as high,
    sum(case when o_orderpriority not in ('1-URGENT', '2-HIGH') then 1 else 0 end) as low
from orders, lineitem
where o_orderkey = l_orderkey and l_shipmode in ('MAIL', 'SHIP') and l_commitdate < l_receiptdate
    and l_shipdate < l_commitdate and l_receiptdate >= date '1994-01-01'
    and l_receiptdate < date '1995-01-01'
group by l_shipmode
order by l_shipmode;
-- customer distribution
select c_count, count(*) as custdist
from (
    select c_custkey, count(o_orderkey) as c_count
    from customer left join orders on c_custkey = o_custkey and o_orderpriority <> '1-URGENT'
    group by c_custkey) c_orders
group by c_count
order by custdist desc, c_count desc;
-- promotion effect
select 100.00 * sum(case when p_type like 'PROMO%' then l_extendedprice * (1 - l_discount) else 0 end)
    / sum(l_extendedprice * (1 - l_discount)) as promo_revenue
from lineitem, part
where l_partkey = p_partkey and l_shipdate >= date '1995-09-01' and l_shipdate < date '1995-10-01';
-- top supplier
with revenue as (
    select l_suppkey as supplier_no, sum(l_extendedprice * (1 - l_discount)) as total
    from lineitem
    where l_shipdate >= date '1996-01-01' and l_shipdate < date '1996-04-01'
    group by l_suppkey)
select s_suppkey, s_name, total
from supplier, revenue
where s_suppkey = supplier_no and total = (select max(total) from revenue)
order by s_suppkey;
-- parts and suppliers
select p_brand, p_type, p_size, count(distinct ps_suppkey) as supplier_cnt
from partsupp, part
where p_partkey = ps_partkey and p_brand <> 'Brand#45' and p_type not like 'MEDIUM POLISHED%'
    and p_size in (49, 14, 23, 45, 19, 3, 36, 9)
    and ps_suppkey not in (select s_suppkey from supplier where s_acctbal < 0)
group by p_brand, p_type, p_size
order by supplier_cnt desc, p_brand, p_type, p_size;
-- small quantity orders
with average as (
    select l_partkey as partkey, 0.2 * avg(l_quantity) as small
    from lineitem
    group by l_partkey)
select sum(l_extendedprice) / 7.0 as avg_yearly
from lineitem, part, average
where p_partkey = l_partkey and partkey = l_partkey and p_brand = 'Brand#23'
    and p_container = 'MED BOX' and l_quantity < small;
-- large volume customers
select c_name, c_custkey, o_orderkey, o_orderdate, o_totalprice, sum(l_quantity)
from customer, orders, lineitem
where o_orderkey in (select l_orderkey from lineitem group by l_orderkey having sum(l_quantity) > 250)
    and c_custkey = o_custkey and o_orderkey = l_orderkey
group by c_name, c_custkey, o_orderkey, o_orderdate, o_totalprice
order by o_totalprice desc, o_orderdate, o_orderkey
limit 100;
-- discounted revenue
select sum(l_extendedprice * (1 - l_discount)) as revenue
from lineitem, part
where p_partkey = l_partkey and l_shipmode in ('AIR', 'REG AIR') and l_shipinstruct = 'DELIVER IN PERSON'
    and ((p_brand = 'Brand#12' and p_container in ('SM CASE', 'SM BOX', 'SM PACK', 'SM PKG')
            and l_quantity between 1 and 11 and p_size between 1 and 5)
        or (p_brand = 'Brand#23' and p_container in ('MED BAG', 'MED BOX', 'MED PKG', 'MED PACK')
            and l_quantity between 10 and 20 and p_size between 1 and 10)
        or (p_brand = 'Brand#34' and p_container in ('LG CASE', 'LG BOX', 'LG PACK', 'LG PKG')
            and l_quantity between 20 and 30 and p_size between 1 and 15));
-- late suppliers
select s_name, count(*) as numwait
from supplier, lineitem l1, orders, nation
where s_suppkey = l1.l_suppkey and o_orderkey = l1.l_orderkey and o_orderstatus = 'F'
    and l1.l_receiptdate > l1.l_commitdate
    and exists (select from lineitem l2 where l2.l_orderkey = l1.l_orderkey and l2.l_suppkey <> l1.l_suppkey)
    and not exists (select from lineitem l3 where l3.l_orderkey = l1.l_orderkey
        and l3.l_suppkey <> l1.l_suppkey and l3.l_receiptdate > l3.l_commitdate)
    and s_nationkey = n_nationkey and n_name = 'NATION 20'
group by s_name
order by numwait desc, s_name
limit 100;
