create function rnd(i bigint, salt int, n bigint) returns bigint
    language sql immutable parallel safe
    return (hashint8(i * 1009 + salt)::bigint & 2147483647) % n;

create table region (r_regionkey int primary key, r_name text not null);
create table nation (n_nationkey int primary key, n_name text not null, n_regionkey int not null);
create table supplier (s_suppkey int primary key, s_name text not null, s_nationkey int not null,
    s_acctbal numeric(12,2) not null);
create table part (p_partkey int primary key, p_name text not null, p_brand text not null,
    p_type text not null, p_size int not null, p_container text not null,
    p_retailprice numeric(12,2) not null);
create table partsupp (ps_partkey int not null, ps_suppkey int not null, ps_availqty int not null,
    ps_supplycost numeric(12,2) not null, primary key (ps_partkey, ps_suppkey));
create table customer (c_custkey int primary key, c_name text not null, c_nationkey int not null,
    c_acctbal numeric(12,2) not null, c_mktsegment text not null);
create table orders (o_orderkey int primary key, o_custkey int not null, o_orderstatus char(1) not null,
    o_totalprice numeric(12,2) not null, o_orderdate date not null, o_orderpriority text not null,
    o_shippriority int not null);
create table lineitem (l_orderkey int not null, l_linenumber int not null, l_partkey int not null,
    l_suppkey int not null, l_quantity numeric(12,2) not null, l_extendedprice numeric(12,2) not null,
    l_discount numeric(12,2) not null, l_tax numeric(12,2) not null, l_returnflag char(1) not null,
    l_linestatus char(1) not null, l_shipdate date not null, l_commitdate date not null,
    l_receiptdate date not null, l_shipinstruct text not null, l_shipmode text not null,
    primary key (l_orderkey, l_linenumber));

insert into region
select i, (array['AFRICA', 'AMERICA', 'ASIA', 'EUROPE', 'MIDDLE EAST'])[i + 1]
from generate_series(0, 4) i;

insert into nation
select i, 'NATION ' || i, i % 5
from generate_series(0, 24) i;

insert into supplier
select i, 'Supplier#' || lpad(i::text, 9, '0'), rnd(i, 1, 25), (rnd(i, 2, 1099999) - 99999) / 100.0
from generate_series(1, {suppliers}) i;

insert into part
select i, 'part ' || i,
    'Brand#' || (rnd(i, 3, 5) + 1) || (rnd(i, 4, 5) + 1),
    (array['STANDARD', 'SMALL', 'MEDIUM', 'LARGE', 'ECONOMY', 'PROMO'])[rnd(i, 5, 6) + 1] || ' '
        || (array['ANODIZED', 'BURNISHED', 'PLATED', 'POLISHED', 'BRUSHED'])[rnd(i, 6, 5) + 1] || ' '
        || (array['TIN', 'NICKEL', 'BRASS', 'STEEL', 'COPPER'])[rnd(i, 7, 5) + 1],
    rnd(i, 8, 50) + 1,
    (array['SM', 'LG', 'MED', 'JUMBO', 'WRAP'])[rnd(i, 9, 5) + 1] || ' '
        || (array['CASE', 'BOX', 'BAG', 'JAR', 'PKG', 'PACK', 'CAN', 'DRUM'])[rnd(i, 10, 8) + 1],
    (90000 + (i / 10) % 20001 + 100 * (i % 1000)) / 100.0
from generate_series(1, {parts}) i;

insert into partsupp
select p, (p + j * ({suppliers} / 4 + (p - 1) / {suppliers})) % {suppliers} + 1,
    rnd(p * 4 + j, 11, 9999) + 1, (rnd(p * 4 + j, 12, 99901) + 100) / 100.0
from generate_series(1, {parts}) p, generate_series(0, 3) j;

insert into customer
select i, 'Customer#' || lpad(i::text, 9, '0'), rnd(i, 13, 25),
    (rnd(i, 14, 1099999) - 99999) / 100.0,
    (array['AUTOMOBILE', 'BUILDING', 'FURNITURE', 'MACHINERY', 'HOUSEHOLD'])[rnd(i, 15, 5) + 1]
from generate_series(1, {customers}) i;

insert into lineitem
select o, l, x.p, (x.p + x.j * ({suppliers} / 4 + (x.p - 1) / {suppliers})) % {suppliers} + 1,
    x.q, x.q * (90000 + (x.p / 10) % 20001 + 100 * (x.p % 1000)) / 100.0,
    rnd(o * 8 + l, 24, 11) / 100.0, rnd(o * 8 + l, 25, 9) / 100.0,
    case when y.receipt <= date '1995-06-17'
        then (array['R', 'A'])[rnd(o * 8 + l, 29, 2) + 1] else 'N' end,
    case when x.ship > date '1995-06-17' then 'O' else 'F' end,
    x.ship, d.od + 30 + rnd(o * 8 + l, 27, 61)::int, y.receipt,
    (array['DELIVER IN PERSON', 'COLLECT COD', 'NONE', 'TAKE BACK RETURN'])[rnd(o * 8 + l, 30, 4) + 1],
    (array['REG AIR', 'AIR', 'RAIL', 'SHIP', 'TRUCK', 'MAIL', 'FOB'])[rnd(o * 8 + l, 31, 7) + 1]
from generate_series(1, {orders}) o
cross join lateral (select date '1992-01-01' + rnd(o, 17, 2406)::int as od) d
cross join lateral generate_series(1, rnd(o, 20, 7)::int + 1) l
cross join lateral (select rnd(o * 8 + l, 21, {parts}) + 1 as p, rnd(o * 8 + l, 22, 4) as j,
    rnd(o * 8 + l, 23, 50) + 1 as q, d.od + 1 + rnd(o * 8 + l, 26, 121)::int as ship) x
cross join lateral (select x.ship + 1 + rnd(o * 8 + l, 28, 30)::int as receipt) y;

insert into orders
select l_orderkey, rnd(l_orderkey, 16, {customers}) + 1,
    case when bool_and(l_linestatus = 'F') then 'F' when bool_and(l_linestatus = 'O') then 'O' else 'P' end,
    sum(l_extendedprice * (1 + l_tax) * (1 - l_discount)),
    date '1992-01-01' + rnd(l_orderkey, 17, 2406)::int,
    (array['1-URGENT', '2-HIGH', '3-MEDIUM', '4-NOT SPECIFIED', '5-LOW'])[rnd(l_orderkey, 18, 5) + 1],
    0
from lineitem
group by l_orderkey;

create index on orders (o_custkey);
create index on orders (o_orderdate);
create index on lineitem (l_partkey);
create index on lineitem (l_shipdate);
create index on partsupp (ps_suppkey);
vacuum analyze;
