//! Turns the URLs of many calls into endpoints: `/users/123/orders/9f3a...` and `/users/456/orders/77b1...` become
//! `/users/{user_id}/orders/{order_id}`. Segments that look like ids are parameters at once; a segment that is a
//! plain word (a slug, a name) becomes one only when many different words stand in the same place and the answers
//! have the same shape, so `/api/users` and `/api/orders` stay two endpoints.

use crate::exchange::Exchange;
use indexmap::IndexMap;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

/// The calls to one endpoint.
#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    /// Upper case.
    pub method: String,
    /// `https://host` or `https://host:port`.
    pub origin: String,
    /// `/users/{user_id}/orders/{order_id}`.
    pub path: String,
    /// The parameter names, in path order.
    pub params: Vec<String>,
    /// For each call: its index in the input slice and its values for `params`, in the same order (percent-decoded).
    pub members: Vec<(usize, Vec<String>)>,
}

/// One path segment of a pattern.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Seg {
    /// A word that is part of the path.
    Lit(String),
    /// A value that looks like an id.
    Id,
    /// A word that stands for many (merged in step 2).
    Slug,
}

/// Calls that share a method, an origin and a pattern.
struct Cluster {
    method: String,
    origin: String,
    pattern: Vec<Seg>,
    /// Indexes into the input, ascending.
    members: Vec<usize>,
}

/// How many first array items, how deep and how many paths one response contributes to its shape key set.
const ARRAY_ITEMS: usize = 5;
const MAX_DEPTH: usize = 6;
const MAX_PATHS: usize = 400;
/// With more clusters than this, each is compared to the first one instead of to every other.
const ALL_PAIRS: usize = 40;

/// Groups calls into endpoints, ordered by the first call of each group.
pub fn group(xs: &[&Exchange]) -> Vec<Group> {
    let segs: Vec<Vec<String>> = xs.iter().map(|x| segments(x)).collect();
    let mut by: IndexMap<(String, String, Vec<Seg>), Vec<usize>> = IndexMap::new();
    for (i, x) in xs.iter().enumerate() {
        let pattern: Vec<Seg> =
            segs[i].iter().map(|s| if is_id_like(&decode(s)) { Seg::Id } else { Seg::Lit(s.clone()) }).collect();
        by.entry((x.method.clone(), x.origin(), pattern)).or_default().push(i);
    }
    let clusters: Vec<Cluster> = by
        .into_iter()
        .map(|((method, origin, pattern), members)| Cluster { method, origin, pattern, members })
        .collect();
    let mut clusters = merge_slugs(clusters, xs, &segs);
    clusters.sort_by_key(|c| c.members.first().copied().unwrap_or(0));
    clusters.into_iter().map(|c| to_group(c, &segs)).collect()
}

/// The non-empty segments of the URL path, as written.
fn segments(x: &Exchange) -> Vec<String> {
    x.url.path().split('/').filter(|s| !s.is_empty()).map(str::to_string).collect()
}

/// Percent-decoding of a path segment (`+` stays `+`).
fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hi = (b[i + 1] as char).to_digit(16);
            let lo = (b[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn is_uuid(s: &str) -> bool {
    s.len() == 36
        && s.char_indices().all(|(i, c)| if matches!(i, 8 | 13 | 18 | 23) { c == '-' } else { c.is_ascii_hexdigit() })
}

/// Whether a path segment looks like a value (an id, a date, a version number, an email) and not a word.
fn is_id_like(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let digits = s.chars().filter(|c| c.is_ascii_digit()).count();
    if digits > 0 && s.chars().all(|c| c.is_ascii_digit() || matches!(c, '-' | '_' | '.' | ':')) {
        return true;
    }
    if is_uuid(s) {
        return true;
    }
    let len = s.chars().count();
    if len >= 12 && s.chars().all(|c| c.is_ascii_hexdigit()) {
        return true;
    }
    if len >= 8 && digits >= 2 && s.chars().any(|c| c.is_alphabetic()) {
        return true;
    }
    if len >= 16 && digits > 0 && s.chars().any(|c| c.is_lowercase()) && s.chars().any(|c| c.is_uppercase()) {
        return true;
    }
    s.contains('@')
}

/// `v2`, `v10`.
fn is_version(s: &str) -> bool {
    s.len() > 1 && s.starts_with('v') && s[1..].chars().all(|c| c.is_ascii_digit())
}

/// The singular of a path word, as a name part: `categories` to `category`, `addresses` to `address`, `users` to
/// `user`, `user-groups` to `user_group`.
fn singular(word: &str) -> String {
    let w = word.to_lowercase();
    let s = if w.len() > 3 && w.ends_with("ies") {
        format!("{}y", &w[..w.len() - 3])
    } else if w.ends_with("ses") || w.ends_with("xes") {
        w[..w.len() - 2].to_string()
    } else if w.ends_with('s') && !w.ends_with("ss") && w.len() > 3 {
        w[..w.len() - 1].to_string()
    } else {
        w
    };
    s.chars().map(|c| if c.is_alphanumeric() { c } else { '_' }).collect()
}

/// The name of the parameter at position `i`, before duplicates are numbered.
fn base_name(pattern: &[Seg], i: usize) -> String {
    if i == 0 {
        return "id".to_string();
    }
    match &pattern[i - 1] {
        Seg::Lit(prev) => {
            let prev = decode(prev).to_lowercase();
            if is_version(&prev) || prev == "api" {
                return "id".to_string();
            }
            let s = singular(&prev);
            if s.is_empty() {
                "id".to_string()
            } else {
                format!("{}_id", s)
            }
        }
        _ => "id".to_string(),
    }
}

fn to_group(c: Cluster, segs: &[Vec<String>]) -> Group {
    let mut parts = Vec::with_capacity(c.pattern.len());
    let mut params = Vec::new();
    let mut at = Vec::new();
    let mut seen: HashMap<String, usize> = HashMap::new();
    for (i, seg) in c.pattern.iter().enumerate() {
        match seg {
            Seg::Lit(word) => parts.push(word.clone()),
            Seg::Id | Seg::Slug => {
                let base = base_name(&c.pattern, i);
                let n = seen.entry(base.clone()).or_insert(0);
                *n += 1;
                let name = if *n == 1 { base } else { format!("{}_{}", base, n) };
                parts.push(format!("{{{}}}", name));
                params.push(name);
                at.push(i);
            }
        }
    }
    let path = if parts.is_empty() { "/".to_string() } else { format!("/{}", parts.join("/")) };
    let members = c.members.iter().map(|&m| (m, at.iter().map(|&p| decode(&segs[m][p])).collect())).collect();
    Group { method: c.method, origin: c.origin, path, params, members }
}

/// Step 2: merges clusters that differ in exactly one word into one with a [`Seg::Slug`] there, when many different
/// words stand in that place and the responses have the same shape.
fn merge_slugs(mut cl: Vec<Cluster>, xs: &[&Exchange], segs: &[Vec<String>]) -> Vec<Cluster> {
    // Every pass turns words into slugs only, so the loop ends.
    loop {
        let mut changed = false;
        let longest = cl.iter().map(|c| c.pattern.len()).max().unwrap_or(0);
        for p in 0..longest {
            let mut by: IndexMap<(String, String, Vec<Seg>), Vec<usize>> = IndexMap::new();
            for (i, c) in cl.iter().enumerate() {
                if matches!(c.pattern.get(p), Some(Seg::Lit(_)) | Some(Seg::Slug)) {
                    let mut pattern = c.pattern.clone();
                    pattern[p] = Seg::Slug;
                    by.entry((c.method.clone(), c.origin.clone(), pattern)).or_default().push(i);
                }
            }
            let mut dead: HashSet<usize> = HashSet::new();
            let mut born: Vec<Cluster> = Vec::new();
            for ((method, origin, pattern), idx) in by {
                let has_word = idx.iter().any(|&i| matches!(cl[i].pattern[p], Seg::Lit(_)));
                if idx.len() < 2 || !has_word {
                    continue;
                }
                let group: Vec<&Cluster> = idx.iter().map(|&i| &cl[i]).collect();
                if !similar(&group, p, xs, segs) {
                    continue;
                }
                let mut members: Vec<usize> = group.iter().flat_map(|c| c.members.iter().copied()).collect();
                members.sort_unstable();
                born.push(Cluster { method, origin, pattern, members });
                dead.extend(idx);
            }
            if !dead.is_empty() {
                changed = true;
                cl =
                    cl.into_iter().enumerate().filter(|(i, _)| !dead.contains(i)).map(|(_, c)| c).chain(born).collect();
            }
        }
        if !changed {
            return cl;
        }
    }
}

/// Whether clusters that differ only at position `p` are one endpoint: 3 or more different words there and either all
/// have JSON responses of the same shape (Jaccard 0.8 or more between their key path sets), or none has JSON and there
/// are 6 or more words.
fn similar(group: &[&Cluster], p: usize, xs: &[&Exchange], segs: &[Vec<String>]) -> bool {
    let mut words: HashSet<&str> = HashSet::new();
    for c in group {
        for &m in &c.members {
            words.insert(segs[m][p].as_str());
        }
    }
    if words.len() < 3 {
        return false;
    }
    let sets: Vec<Option<HashSet<String>>> = group.iter().map(|c| cluster_paths(c, xs)).collect();
    let with = sets.iter().filter(|s| s.is_some()).count();
    if with == 0 {
        return words.len() >= 6;
    }
    if with != group.len() {
        return false;
    }
    let sets: Vec<&HashSet<String>> = sets.iter().flatten().collect();
    if sets.len() <= ALL_PAIRS {
        for i in 0..sets.len() {
            for j in (i + 1)..sets.len() {
                if jaccard(sets[i], sets[j]) < 0.8 {
                    return false;
                }
            }
        }
    } else {
        for j in 1..sets.len() {
            if jaccard(sets[0], sets[j]) < 0.8 {
                return false;
            }
        }
    }
    true
}

fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    let inter = a.intersection(b).count();
    let union = a.len() + b.len() - inter;
    if union == 0 {
        1.0
    } else {
        inter as f64 / union as f64
    }
}

/// The key paths of the JSON responses of a cluster (`data[].id`), or `None` when none of its calls has one.
fn cluster_paths(c: &Cluster, xs: &[&Exchange]) -> Option<HashSet<String>> {
    let mut out = HashSet::new();
    let mut any = false;
    for &m in &c.members {
        if let Some(json) = xs[m].resp_body.as_ref().and_then(|b| b.json.as_ref()) {
            any = true;
            key_paths(json, "", 0, &mut out);
        }
    }
    if any {
        Some(out)
    } else {
        None
    }
}

/// The key paths of a JSON value: first 5 array items, depth 6, at most 400 paths.
fn key_paths(v: &Value, prefix: &str, depth: usize, out: &mut HashSet<String>) {
    if depth > MAX_DEPTH || out.len() >= MAX_PATHS {
        return;
    }
    match v {
        Value::Object(map) => {
            for (k, child) in map {
                if out.len() >= MAX_PATHS {
                    return;
                }
                let path = if prefix.is_empty() { k.clone() } else { format!("{}.{}", prefix, k) };
                out.insert(path.clone());
                key_paths(child, &path, depth + 1, out);
            }
        }
        Value::Array(items) => {
            let path = format!("{}[]", prefix);
            for child in items.iter().take(ARRAY_ITEMS) {
                key_paths(child, &path, depth + 1, out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exchange::Raw;

    fn ex(method: &str, url: &str, json: Option<&str>) -> Exchange {
        Raw {
            method: method.into(),
            url: url.into(),
            status: 200,
            resp_body: json.map(|j| ("application/json".to_string(), j.to_string())),
            ..Raw::default()
        }
        .into_exchange(0)
        .unwrap()
    }

    fn run(xs: &[Exchange]) -> Vec<Group> {
        let refs: Vec<&Exchange> = xs.iter().collect();
        group(&refs)
    }

    fn paths(gs: &[Group]) -> Vec<String> {
        gs.iter().map(|g| g.path.clone()).collect()
    }

    fn get(url: &str) -> Exchange {
        ex("GET", url, None)
    }

    #[test]
    fn numeric_ids_become_one_endpoint_with_values() {
        let gs = run(&[get("https://a.test/users/1"), get("https://a.test/users/22")]);
        assert_eq!(gs.len(), 1);
        assert_eq!(gs[0].path, "/users/{user_id}");
        assert_eq!(gs[0].params, vec!["user_id"]);
        assert_eq!(gs[0].method, "GET");
        assert_eq!(gs[0].origin, "https://a.test");
        assert_eq!(gs[0].members, vec![(0, vec!["1".to_string()]), (1, vec!["22".to_string()])]);
    }

    #[test]
    fn nested_ids_are_named_after_their_previous_word() {
        let gs = run(&[get("https://a.test/users/1/orders/9"), get("https://a.test/users/2/orders/10")]);
        assert_eq!(paths(&gs), vec!["/users/{user_id}/orders/{order_id}"]);
        assert_eq!(gs[0].params, vec!["user_id", "order_id"]);
        assert_eq!(gs[0].members[1], (1, vec!["2".to_string(), "10".to_string()]));
    }

    #[test]
    fn uuids_hashes_dates_mixed_codes_and_emails_are_ids() {
        for seg in [
            "123e4567-e89b-12d3-a456-426614174000",
            "9f3a1c2b4d5e",
            "2024-05-06",
            "1.2.3",
            "abc12345",
            "Ab3dEfGh1JkLmNoP",
            "bob@x.com",
            "ann%40x.com",
            "10:30",
        ] {
            let gs = run(&[get(&format!("https://a.test/things/{}", seg))]);
            assert_eq!(paths(&gs), vec!["/things/{thing_id}"], "{seg}");
        }
    }

    #[test]
    fn words_versions_and_short_codes_stay_literal() {
        for seg in ["me", "v2", "v10", "search", "oauth2", "alpha-beta", "user_groups", "abcdefghij", "a1"] {
            let gs = run(&[get(&format!("https://a.test/{}/x", seg))]);
            assert_eq!(paths(&gs), vec![format!("/{}/x", seg)], "{seg}");
        }
    }

    #[test]
    fn a_version_prefix_is_kept_and_the_id_after_it_is_plain_id() {
        let gs = run(&[get("https://a.test/v2/users/5"), get("https://a.test/v2/users/6")]);
        assert_eq!(paths(&gs), vec!["/v2/users/{user_id}"]);
        let gs = run(&[get("https://a.test/v1/7"), get("https://a.test/api/7")]);
        assert_eq!(paths(&gs), vec!["/v1/{id}", "/api/{id}"]);
    }

    #[test]
    fn percent_encoded_values_are_decoded() {
        let gs = run(&[get("https://a.test/accounts/ann%40x.com"), get("https://a.test/accounts/bob@y.org")]);
        assert_eq!(gs.len(), 1);
        assert_eq!(gs[0].members[0].1, vec!["ann@x.com"]);
        assert_eq!(gs[0].members[1].1, vec!["bob@y.org"]);
    }

    #[test]
    fn param_names_are_singular_and_duplicates_are_numbered() {
        let gs = run(&[
            get("https://a.test/categories/1"),
            get("https://a.test/addresses/1"),
            get("https://a.test/boxes/1"),
            get("https://a.test/class/1"),
            get("https://a.test/user-groups/1"),
            get("https://a.test/orders/1/orders/2"),
            get("https://a.test/5/6"),
            get("https://b.test/ids/1"),
        ]);
        assert_eq!(
            paths(&gs),
            vec![
                "/categories/{category_id}",
                "/addresses/{address_id}",
                "/boxes/{box_id}",
                "/class/{class_id}",
                "/user-groups/{user_group_id}",
                "/orders/{order_id}/orders/{order_id_2}",
                "/{id}/{id_2}",
                "/ids/{ids_id}",
            ]
        );
        assert_eq!(gs[6].params, vec!["id", "id_2"]);
    }

    #[test]
    fn method_and_origin_keep_endpoints_apart() {
        let gs = run(&[
            ex("GET", "https://a.test/users/1", None),
            ex("POST", "https://a.test/users/1", None),
            ex("GET", "https://b.test/users/1", None),
            ex("GET", "https://a.test/users/2", None),
        ]);
        assert_eq!(gs.len(), 3);
        assert_eq!((gs[0].method.as_str(), gs[0].origin.as_str()), ("GET", "https://a.test"));
        assert_eq!(gs[0].members.iter().map(|m| m.0).collect::<Vec<_>>(), vec![0, 3]);
        assert_eq!(gs[1].method, "POST");
        assert_eq!(gs[2].origin, "https://b.test");
    }

    #[test]
    fn groups_come_in_the_order_of_their_first_member() {
        let gs = run(&[get("https://a.test/z"), get("https://a.test/a"), get("https://a.test/z")]);
        assert_eq!(paths(&gs), vec!["/z", "/a"]);
        assert_eq!(gs[0].members.iter().map(|m| m.0).collect::<Vec<_>>(), vec![0, 2]);
    }

    #[test]
    fn the_root_and_trailing_slashes_are_handled() {
        let gs = run(&[
            get("https://a.test/"),
            get("https://a.test"),
            get("https://a.test/users/"),
            get("https://a.test/users"),
        ]);
        assert_eq!(paths(&gs), vec!["/", "/users"]);
        assert!(gs[0].params.is_empty());
    }

    fn page(slug: &str, json: &str) -> Exchange {
        ex("GET", &format!("https://a.test/blog/{}", slug), Some(json))
    }

    #[test]
    fn slugs_with_the_same_response_shape_merge() {
        let j = r#"{"title":"t","body":"b","tags":["x"]}"#;
        let gs = run(&[page("alpha", j), page("beta", j), page("gamma", j)]);
        assert_eq!(gs.len(), 1);
        assert_eq!(gs[0].path, "/blog/{blog_id}");
        assert_eq!(gs[0].params, vec!["blog_id"]);
        let vals: Vec<&str> = gs[0].members.iter().map(|m| m.1[0].as_str()).collect();
        assert_eq!(vals, vec!["alpha", "beta", "gamma"]);
    }

    #[test]
    fn slug_shapes_that_overlap_by_four_fifths_still_merge() {
        let a = r#"{"a":1,"b":1,"c":1,"d":1,"e":1}"#;
        let b = r#"{"a":1,"b":1,"c":1,"d":1,"e":1,"f":1}"#;
        let gs = run(&[page("alpha", a), page("beta", a), page("gamma", b)]);
        assert_eq!(paths(&gs), vec!["/blog/{blog_id}"]);
    }

    #[test]
    fn slug_shapes_that_differ_do_not_merge() {
        let a = r#"{"a":1,"b":1,"c":1,"d":1}"#;
        let b = r#"{"a":1,"b":1,"x":1,"y":1}"#;
        let gs = run(&[page("alpha", a), page("beta", a), page("gamma", b)]);
        assert_eq!(gs.len(), 3);
    }

    #[test]
    fn different_resources_under_one_prefix_stay_apart() {
        let gs = run(&[
            ex("GET", "https://a.test/api/users", Some(r#"{"id":1,"name":"a","email":"e"}"#)),
            ex("GET", "https://a.test/api/orders", Some(r#"{"id":1,"total":3,"items":[]}"#)),
            ex("GET", "https://a.test/api/products", Some(r#"{"sku":"s","price":1,"stock":2}"#)),
        ]);
        assert_eq!(paths(&gs), vec!["/api/users", "/api/orders", "/api/products"]);
    }

    #[test]
    fn two_words_are_not_enough_to_merge() {
        let j = r#"{"title":"t"}"#;
        let gs = run(&[page("alpha", j), page("beta", j)]);
        assert_eq!(gs.len(), 2);
    }

    #[test]
    fn without_json_six_words_merge_and_five_do_not() {
        let words = ["alpha", "beta", "gamma", "delta", "omega", "sigma"];
        let six: Vec<Exchange> = words.iter().map(|w| get(&format!("https://a.test/wiki/{}", w))).collect();
        assert_eq!(paths(&run(&six)), vec!["/wiki/{wiki_id}"]);
        let five: Vec<Exchange> = words[..5].iter().map(|w| get(&format!("https://a.test/wiki/{}", w))).collect();
        assert_eq!(run(&five).len(), 5);
    }

    #[test]
    fn a_mix_of_json_and_no_json_does_not_merge() {
        let gs = run(&[page("alpha", r#"{"a":1}"#), page("beta", r#"{"a":1}"#), get("https://a.test/blog/gamma")]);
        assert_eq!(gs.len(), 3);
    }

    #[test]
    fn a_slug_and_an_id_in_one_path_both_become_parameters() {
        let j = r#"{"name":"n","price":1}"#;
        let gs = run(&[
            ex("GET", "https://a.test/shop/alpha/items/1", Some(j)),
            ex("GET", "https://a.test/shop/beta/items/2", Some(j)),
            ex("GET", "https://a.test/shop/gamma/items/3", Some(j)),
        ]);
        assert_eq!(paths(&gs), vec!["/shop/{shop_id}/items/{item_id}"]);
        assert_eq!(gs[0].members[2], (2, vec!["gamma".to_string(), "3".to_string()]));
    }

    #[test]
    fn slug_key_paths_look_into_arrays_and_stop_at_the_caps() {
        let v: Value = serde_json::from_str(r#"{"data":[{"id":1,"a":{"b":1}},{"id":2}],"n":1}"#).unwrap();
        let mut out = HashSet::new();
        key_paths(&v, "", 0, &mut out);
        let mut got: Vec<String> = out.into_iter().collect();
        got.sort();
        assert_eq!(got, vec!["data", "data[].a", "data[].a.b", "data[].id", "n"]);

        let mut wide = serde_json::Map::new();
        for i in 0..1000 {
            wide.insert(format!("k{}", i), Value::Null);
        }
        let mut out = HashSet::new();
        key_paths(&Value::Object(wide), "", 0, &mut out);
        assert_eq!(out.len(), MAX_PATHS);
    }

    #[test]
    fn decode_leaves_broken_escapes_alone() {
        assert_eq!(decode("a%20b"), "a b");
        assert_eq!(decode("100%"), "100%");
        assert_eq!(decode("%zz"), "%zz");
        assert_eq!(decode("a+b"), "a+b");
    }
}
