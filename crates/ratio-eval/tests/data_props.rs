//! Properties of the stage 8 built-ins against models written in Rust: random lists and maps
//! through `list.*` and `map.*` compared with `Vec` and `BTreeMap`, and random JSON documents
//! through `json.diff`, `json.patch`, `json.set`, `json.get`, `json.merge` and `json.equal`.

#![allow(
    clippy::many_single_char_names,
    clippy::too_many_lines,
    clippy::single_match_else,
    reason = "test programs and models name values briefly, and a corpus runner reads best whole"
)]

mod support;

use std::collections::BTreeMap;

use ratio_eval::{Outcome, Value, to_json_text};
use support::{prepared, run};

const SOURCE: &str = r#"
fn p_sort(xs: [Int]) -> [Int] { list.sort(xs) }
fn p_sort_desc(xs: [Int]) -> [Int] { list.sort_by(xs, |x| x % 10, true) }
fn p_unique(xs: [Int]) -> [Int] { list.unique(xs) }
fn p_unique_by(xs: [Int]) -> [Int] { list.unique_by(xs, |x| x % 5) }
fn p_reverse(xs: [Int]) -> [Int] { list.reverse(xs) }
fn p_chunk(xs: [Int], k: Int) -> [[Int]] { list.chunk(xs, k) }
fn p_window(xs: [Int], k: Int) -> [[Int]] { list.window(xs, k) }
fn p_slice(xs: [Int], a: Int, b: Int) -> [Int] { list.slice(xs, a, b) }
fn p_insert(xs: [Int], i: Int) -> [Int] { list.insert(xs, i, 99) }
fn p_remove(xs: [Int], i: Int) -> [Int] { list.remove_at(xs, i) }
fn p_flatten(xs: [Int], k: Int) -> [Int] { list.flatten(list.chunk(xs, k)) }
fn p_zip(xs: [Int], k: Int) -> [(Int, Int)] { list.zip(xs, list.drop(xs, k)) }
fn p_index_of(xs: [Int], k: Int) -> Int? { list.index_of(xs, k) }
fn p_extremes(xs: [Int]) -> (Int?, Int?, Int) { (list.min(xs), list.max(xs), list.sum(xs)) }
fn p_find(xs: [Int], k: Int) -> (Int?, Int?, Bool, Bool, Int) {
  (list.find(xs, |x| x > k), list.find_index(xs, |x| x > k), list.any(xs, |x| x > k), list.all(xs, |x| x > k), list.count(xs, |x| x > k))
}
fn p_partition(xs: [Int], k: Int) -> ([Int], [Int]) { list.partition(xs, |x| x < k) }
fn p_take_while(xs: [Int], k: Int) -> [Int] { list.take_while(xs, |x| x < k) }
fn p_group(xs: [Int]) -> Map<Int, [Int]> { list.group_by(xs, |x| x % 5) }
fn p_count_by(xs: [Int]) -> Map<Int, Int> { map.count_by(xs, |x| x % 5) }
fn p_from(xs: [Int]) -> Map<Int, Int> { map.from(xs.map(|x| (x % 7, x))) }
fn p_merge(xs: [Int], ys: [Int]) -> Map<Int, Int> { map.merge(map.from(xs.map(|x| (x % 11, x))), map.from(ys.map(|y| (y % 11, 100 + y)))) }
fn p_edit(xs: [Int], k: Int) -> Map<Int, Int> {
  let m = map.from(xs.map(|x| (x % 13, x)))
  map.update(map.remove(map.set(m, k, -1), k + 1), k + 2, |v| v + 1000, 7)
}
fn p_entries(xs: [Int]) -> ([Int], [Int], [(Int, Int)]) {
  let m = map.from(xs.map(|x| (x % 9, x)))
  (map.keys(m), map.values(m), map.entries(m))
}
fn p_map_filter(xs: [Int]) -> Map<Int, Int> {
  map.filter(map.map_values(map.from(xs.map(|x| (x, x))), |v| v * 2), |k, v| k % 3 == 0)
}
fn p_has(xs: [Int], k: Int) -> (Bool, Int?, Int) {
  let m = map.from(xs.map(|x| (x, x)))
  (map.has(m, k), map.get(m, k), map.len(m))
}
fn p_json_trip(a: Str, b: Str) -> Bool {
  let x = json.parse(a)
  let y = json.parse(b)
  json.equal(json.patch(x, json.diff(x, y)), y)
}
fn p_json_set_get(a: Str, p: Str) -> Bool {
  let x = json.parse(a)
  let v = json.parse("[\"set\", 1]")
  json.equal(json.get(json.set(x, p, v), p) ?? json.parse("null"), v)
}
fn p_json_merge_idem(a: Str, b: Str) -> Bool {
  let x = json.parse(a)
  let patch = json.parse(b)
  let once = json.merge(x, patch)
  json.equal(json.merge(once, patch), once)
}
"#;

/// A small xorshift generator: the tests are deterministic.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> i64 {
        i64::try_from(self.next() % n).unwrap()
    }
}

fn list_text(xs: &[i64]) -> String {
    format!(
        "[{}]",
        xs.iter().map(i64::to_string).collect::<Vec<_>>().join(",")
    )
}

fn nested_text(rows: &[Vec<i64>]) -> String {
    format!(
        "[{}]",
        rows.iter()
            .map(|r| list_text(r))
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn map_text(m: &BTreeMap<i64, i64>) -> String {
    format!(
        "{{{}}}",
        m.iter()
            .map(|(k, v)| format!("\"{k}\":{v}"))
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn ints(xs: &[i64]) -> Value {
    Value::list(xs.iter().copied().map(Value::int).collect()).unwrap()
}

fn call(
    p: &std::sync::Arc<ratio_eval::Prepared>,
    name: &str,
    args: Vec<Value>,
) -> Result<String, String> {
    match run(p, name, args).outcome {
        Outcome::Returned(v) => Ok(to_json_text(p, &v)),
        Outcome::Raised(e) => Err(e.kind(p)),
    }
}

fn random_list(rng: &mut Rng) -> Vec<i64> {
    let n = rng.below(40);
    (0..n).map(|_| rng.below(30) - 5).collect()
}

/// `list.slice`'s index: negative counts from the end, clamped to the list.
fn clamp(i: i64, len: usize) -> usize {
    let len = i64::try_from(len).unwrap();
    usize::try_from(if i < 0 { (len + i).max(0) } else { i.min(len) }).unwrap()
}

#[test]
fn lists_agree_with_vec() {
    let p = prepared(SOURCE);
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for _ in 0..150 {
        let xs = random_list(&mut rng);
        let k = rng.below(8) - 1;
        let a = rng.below(60) - 30;
        let b = rng.below(60) - 30;
        let n = xs.len();
        let at = |name: &str, extra: Vec<i64>| {
            let mut args = vec![ints(&xs)];
            args.extend(extra.into_iter().map(Value::int));
            call(&p, name, args)
        };

        let mut sorted = xs.clone();
        sorted.sort_unstable();
        assert_eq!(at("p_sort", vec![]), Ok(list_text(&sorted)), "{xs:?}");
        let mut desc = xs.clone();
        desc.sort_by_key(|x| std::cmp::Reverse(x % 10));
        // `x % 10` of a negative number is negative, as in Ratio.
        assert_eq!(at("p_sort_desc", vec![]), Ok(list_text(&desc)), "{xs:?}");
        let mut seen = Vec::new();
        for x in &xs {
            if !seen.contains(x) {
                seen.push(*x);
            }
        }
        assert_eq!(at("p_unique", vec![]), Ok(list_text(&seen)), "{xs:?}");
        let mut keys = Vec::new();
        let mut firsts = Vec::new();
        for x in &xs {
            if !keys.contains(&(x % 5)) {
                keys.push(x % 5);
                firsts.push(*x);
            }
        }
        assert_eq!(at("p_unique_by", vec![]), Ok(list_text(&firsts)), "{xs:?}");
        let mut rev = xs.clone();
        rev.reverse();
        assert_eq!(at("p_reverse", vec![]), Ok(list_text(&rev)));

        let size = usize::try_from(k).ok().filter(|s| *s >= 1);
        match size {
            Some(s) => {
                let chunks: Vec<Vec<i64>> = xs.chunks(s).map(<[i64]>::to_vec).collect();
                assert_eq!(
                    at("p_chunk", vec![k]),
                    Ok(nested_text(&chunks)),
                    "{xs:?} {k}"
                );
                let windows: Vec<Vec<i64>> = xs.windows(s).map(<[i64]>::to_vec).collect();
                assert_eq!(
                    at("p_window", vec![k]),
                    Ok(nested_text(&windows)),
                    "{xs:?} {k}"
                );
                let flat: Vec<i64> = chunks.concat();
                assert_eq!(at("p_flatten", vec![k]), Ok(list_text(&flat)));
                let zipped: Vec<String> = xs
                    .iter()
                    .zip(xs.iter().skip(s))
                    .map(|(x, y)| format!("[{x},{y}]"))
                    .collect();
                assert_eq!(at("p_zip", vec![k]), Ok(format!("[{}]", zipped.join(","))));
            }
            None => {
                assert_eq!(at("p_chunk", vec![k]), Err("Invalid".to_owned()));
                assert_eq!(at("p_window", vec![k]), Err("Invalid".to_owned()));
            }
        }

        let (from, to) = (clamp(a, n), clamp(b, n));
        let sliced: Vec<i64> = if to > from {
            xs[from..to].to_vec()
        } else {
            Vec::new()
        };
        assert_eq!(
            at("p_slice", vec![a, b]),
            Ok(list_text(&sliced)),
            "{xs:?} {a} {b}"
        );

        let i = rng.below(n as u64 + 3) - 1;
        let want_insert = usize::try_from(i).ok().filter(|i| *i <= n).map(|i| {
            let mut v = xs.clone();
            v.insert(i, 99);
            list_text(&v)
        });
        assert_eq!(
            at("p_insert", vec![i]),
            want_insert.ok_or_else(|| "Invalid".to_owned())
        );
        let want_remove = usize::try_from(i).ok().filter(|i| *i < n).map(|i| {
            let mut v = xs.clone();
            v.remove(i);
            list_text(&v)
        });
        assert_eq!(
            at("p_remove", vec![i]),
            want_remove.ok_or_else(|| "Invalid".to_owned())
        );

        let found = xs.iter().position(|x| *x == k);
        assert_eq!(
            at("p_index_of", vec![k]),
            Ok(found.map_or_else(|| "null".to_owned(), |i| i.to_string()))
        );
        let (lo, hi) = (xs.iter().min(), xs.iter().max());
        assert_eq!(
            at("p_extremes", vec![]),
            Ok(format!(
                "[{},{},{}]",
                lo.map_or_else(|| "null".to_owned(), ToString::to_string),
                hi.map_or_else(|| "null".to_owned(), ToString::to_string),
                xs.iter().sum::<i64>()
            ))
        );
        let first = xs.iter().find(|x| **x > k);
        let first_at = xs.iter().position(|x| *x > k);
        assert_eq!(
            at("p_find", vec![k]),
            Ok(format!(
                "[{},{},{},{},{}]",
                first.map_or_else(|| "null".to_owned(), ToString::to_string),
                first_at.map_or_else(|| "null".to_owned(), |i| i.to_string()),
                xs.iter().any(|x| *x > k),
                xs.iter().all(|x| *x > k),
                xs.iter().filter(|x| **x > k).count()
            ))
        );
        let (yes, no): (Vec<i64>, Vec<i64>) = xs.iter().partition(|x| **x < k);
        assert_eq!(
            at("p_partition", vec![k]),
            Ok(format!("[{},{}]", list_text(&yes), list_text(&no)))
        );
        let prefix: Vec<i64> = xs.iter().copied().take_while(|x| *x < k).collect();
        assert_eq!(at("p_take_while", vec![k]), Ok(list_text(&prefix)));

        let mut groups: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
        for x in &xs {
            groups.entry(x % 5).or_default().push(*x);
        }
        let want = format!(
            "{{{}}}",
            groups
                .iter()
                .map(|(k, v)| format!("\"{k}\":{}", list_text(v)))
                .collect::<Vec<_>>()
                .join(",")
        );
        assert_eq!(at("p_group", vec![]), Ok(want));
        let mut counts: BTreeMap<i64, i64> = BTreeMap::new();
        for x in &xs {
            *counts.entry(x % 5).or_default() += 1;
        }
        assert_eq!(at("p_count_by", vec![]), Ok(map_text(&counts)));
    }
}

#[test]
fn maps_agree_with_btreemap() {
    let p = prepared(SOURCE);
    let mut rng = Rng(0xD1B5_4A32_D192_ED03);
    for _ in 0..150 {
        let xs = random_list(&mut rng);
        let ys = random_list(&mut rng);
        let k = rng.below(14) - 1;
        let model = |list: &[i64], m: i64, add: i64| {
            let mut out = BTreeMap::new();
            for x in list {
                out.insert(x % m, x + add);
            }
            out
        };
        assert_eq!(
            call(&p, "p_from", vec![ints(&xs)]),
            Ok(map_text(&model(&xs, 7, 0))),
            "{xs:?}"
        );
        let mut merged = model(&xs, 11, 0);
        merged.extend(model(&ys, 11, 100));
        assert_eq!(
            call(&p, "p_merge", vec![ints(&xs), ints(&ys)]),
            Ok(map_text(&merged)),
            "{xs:?} {ys:?}"
        );
        let mut edited = model(&xs, 13, 0);
        edited.insert(k, -1);
        edited.remove(&(k + 1));
        let bumped = edited.get(&(k + 2)).map_or(7, |v| *v) + 1000;
        edited.insert(k + 2, bumped);
        assert_eq!(
            call(&p, "p_edit", vec![ints(&xs), Value::int(k)]),
            Ok(map_text(&edited)),
            "{xs:?} {k}"
        );
        let m9 = model(&xs, 9, 0);
        assert_eq!(
            call(&p, "p_entries", vec![ints(&xs)]),
            Ok(format!(
                "[{},{},[{}]]",
                list_text(&m9.keys().copied().collect::<Vec<_>>()),
                list_text(&m9.values().copied().collect::<Vec<_>>()),
                m9.iter()
                    .map(|(k, v)| format!("[{k},{v}]"))
                    .collect::<Vec<_>>()
                    .join(",")
            ))
        );
        let ident: BTreeMap<i64, i64> = xs.iter().map(|x| (*x, *x)).collect();
        let filtered: BTreeMap<i64, i64> = ident
            .iter()
            .filter(|(k, _)| **k % 3 == 0)
            .map(|(k, v)| (*k, v * 2))
            .collect();
        assert_eq!(
            call(&p, "p_map_filter", vec![ints(&xs)]),
            Ok(map_text(&filtered))
        );
        assert_eq!(
            call(&p, "p_has", vec![ints(&xs), Value::int(k)]),
            Ok(format!(
                "[{},{},{}]",
                ident.contains_key(&k),
                ident
                    .get(&k)
                    .map_or_else(|| "null".to_owned(), ToString::to_string),
                ident.len()
            ))
        );
    }
}

fn random_json(rng: &mut Rng, depth: u32) -> String {
    match if depth == 0 {
        rng.below(4)
    } else {
        rng.below(7)
    } {
        0 => rng.below(5).to_string(),
        1 => format!("\"s{}\"", rng.below(4)),
        2 => (rng.below(2) == 0).to_string(),
        3 => "null".to_owned(),
        4 | 5 => {
            let items: Vec<String> = (0..rng.below(5))
                .map(|_| random_json(rng, depth - 1))
                .collect();
            format!("[{}]", items.join(","))
        }
        _ => {
            let mut keys: Vec<i64> = (0..rng.below(5)).map(|_| rng.below(6)).collect();
            keys.sort_unstable();
            keys.dedup();
            let items: Vec<String> = keys
                .iter()
                .map(|k| format!("\"k{k}\":{}", random_json(rng, depth - 1)))
                .collect();
            format!("{{{}}}", items.join(","))
        }
    }
}

#[test]
fn json_documents_round_trip_through_diff_patch_and_text() {
    let p = prepared(SOURCE);
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    for _ in 0..300 {
        let (a, b) = (random_json(&mut rng, 4), random_json(&mut rng, 4));
        assert_eq!(
            call(&p, "p_json_trip", vec![Value::text(&a), Value::text(&b)]),
            Ok("true".to_owned()),
            "diff/patch of {a} to {b}"
        );
        let patch = random_json(&mut rng, 3);
        assert_eq!(
            call(
                &p,
                "p_json_merge_idem",
                vec![Value::text(&a), Value::text(&patch)]
            ),
            Ok("true".to_owned()),
            "merge of {patch} into {a}"
        );
    }
}

#[test]
fn json_set_then_get_gives_the_value_back() {
    let p = prepared(SOURCE);
    let mut rng = Rng(0x1234_5678_9ABC_DEF1);
    for _ in 0..200 {
        let doc = format!(
            "{{\"k0\":{},\"k1\":[{},{}]}}",
            random_json(&mut rng, 3),
            random_json(&mut rng, 2),
            random_json(&mut rng, 2)
        );
        for path in ["/k0", "/k1/0", "/k1/1", "/k1/2", "/new", "/k0/deep"] {
            let got = call(
                &p,
                "p_json_set_get",
                vec![Value::text(&doc), Value::text(path)],
            );
            // Setting under a scalar (or past the array's end) is refused; everything else sticks.
            if let Ok(text) = &got {
                assert_eq!(text, "true", "{doc} at {path}");
            } else {
                assert_eq!(got, Err("Invalid".to_owned()), "{doc} at {path}");
            }
        }
    }
}
