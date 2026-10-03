//! The analytic set `rpg bench` times: a TPC-H style schema, a generator for it written in SQL,
//! and twenty queries over it.
//!
//! The generator is ours, so that nothing has to be downloaded or built before a run. Every
//! value comes from `hashint8` of the row number and a salt, with no `random()` and no state, so
//! two servers that compute correctly load the same rows, and two servers that answer a query
//! correctly print the same bytes. At scale factor 1 there are 10,000 suppliers, 200,000 parts,
//! 800,000 part suppliers, 150,000 customers, 1,500,000 orders and about 6,000,000 order lines,
//! the counts TPC-H uses. The values follow TPC-H's ranges loosely and are not meant to match
//! its distributions, only to give the planner and executor the same kind of work.
//!
//! The queries are written in the style of TPC-H's, joins, grouping, correlated and uncorrelated
//! subqueries, `exists` and `not exists`, `case` inside aggregates and `like` on text, each with
//! an order that leaves no ties, so that the output can be compared byte for byte. Every number
//! they compute is `numeric`, which has no rounding that could depend on the order rows arrive.

/// The schema and the generator, with the row counts left as `{suppliers}` and the like.
const LOAD: &str = include_str!("analytic/load.sql");

/// The queries, each after a `-- name` line.
const QUERIES: &str = include_str!("analytic/queries.sql");

/// The SQL that creates and loads the schema at a scale factor.
#[must_use]
pub fn load(scale_factor: f64) -> String {
    let rows = |base: f64, least: f64| format!("{:.0}", (base * scale_factor).round().max(least));
    // The part supplier formula spreads a part over four suppliers, so it needs four of them.
    LOAD.replace("{suppliers}", &rows(10_000.0, 4.0))
        .replace("{parts}", &rows(200_000.0, 1.0))
        .replace("{customers}", &rows(150_000.0, 1.0))
        .replace("{orders}", &rows(1_500_000.0, 1.0))
}

/// The queries by name, in the order they run.
#[must_use]
pub fn queries() -> Vec<(&'static str, &'static str)> {
    let mut found = Vec::new();
    let mut rest = QUERIES;
    while let Some(start) = rest.find("-- ") {
        let after = &rest[start + 3..];
        let (name, body) = after.split_once('\n').unwrap_or((after, ""));
        let end = body.find("\n-- ").map_or(body.len(), |e| e + 1);
        found.push((name.trim(), body[..end].trim()));
        rest = &body[end..];
    }
    found
}

/// A short fingerprint of a query's output, 64 bit FNV-1a in hex, so that two servers' answers
/// can be compared without keeping them.
#[must_use]
pub fn fingerprint(text: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn there_are_twenty_named_queries_and_none_is_empty() {
        let queries = queries();
        assert_eq!(queries.len(), 20);
        assert_eq!(queries[0].0, "pricing summary");
        assert!(queries[0].1.starts_with("select l_returnflag"));
        assert_eq!(queries[19].0, "late suppliers");
        for (name, body) in &queries {
            assert!(body.ends_with(';'), "{name}: {body}");
            assert!(!body.contains("\n-- "), "{name}: {body}");
        }
        let mut names: Vec<_> = queries.iter().map(|q| q.0).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 20);
    }

    #[test]
    fn the_load_has_every_count_filled_in() {
        let sql = load(1.0);
        assert!(!sql.contains('{'), "a count was left out");
        assert!(sql.contains("from generate_series(1, 1500000) o"));
        assert!(sql.contains("generate_series(1, 10000) i"));
        let tiny = load(0.0001);
        assert!(tiny.contains("% 4 + 1"), "{tiny}");
    }

    #[test]
    fn the_fingerprint_is_fnv_1a() {
        assert_eq!(fingerprint(""), "cbf29ce484222325");
        assert_eq!(fingerprint("a"), "af63dc4c8601ec8c");
    }
}
