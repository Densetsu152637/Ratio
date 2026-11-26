//! Property tests of the representations (chapter 06, `data`): persistent lists and maps
//! agree with a simple model and stay balanced, and **no container with a labelled leaf has a
//! clear summary bit**, whatever operations built it, including labels a replay adds and
//! values restored from a cache entry.

use std::collections::BTreeMap;

use crate::heap::Heap;
use crate::labels::Labels;
use crate::plist::List;
use crate::pmap::Map;
use crate::value::Value;

/// A small deterministic generator (xorshift64*), so failures reproduce.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }

    fn labels(&mut self) -> Labels {
        if self.below(5) == 0 {
            Labels::source(u32::try_from(self.below(3)).unwrap_or(0))
        } else {
            Labels::NONE
        }
    }

    fn leaf(&mut self) -> Value {
        let l = self.labels();
        match self.below(3) {
            0 => Value::int(i64::try_from(self.below(100)).unwrap_or(0)).with_labels(l),
            1 => Value::text(["a", "bb", "ccc"][usize::try_from(self.below(3)).unwrap_or(0)])
                .with_labels(l),
            _ => Value::bool(self.below(2) == 0).with_labels(l),
        }
    }
}

fn holds(v: &Value) -> bool {
    v.summaries_hold() && v.quick().contains(v.scan())
}

#[test]
fn lists_agree_with_a_model_and_keep_their_summaries() {
    let heap = Heap::unmetered();
    for seed in 1..40u64 {
        let mut rng = Rng(seed * 7919);
        let mut list = List::empty();
        let mut model: Vec<Value> = Vec::new();
        for _ in 0..300 {
            match rng.below(5) {
                0 | 1 => {
                    let v = rng.leaf();
                    list = list.push(&heap, v.clone()).unwrap();
                    model.push(v);
                }
                2 => {
                    let n = rng.below(40);
                    let extra: Vec<Value> = (0..n).map(|_| rng.leaf()).collect();
                    let other = List::from_vec(&heap, extra.clone()).unwrap();
                    list = list.concat(&heap, &other).unwrap();
                    model.extend(extra);
                }
                3 if !model.is_empty() => {
                    let a = rng.below(model.len() as u64);
                    let b = a + rng.below(model.len() as u64 - a + 1);
                    list = list.slice(&heap, a, b).unwrap();
                    model =
                        model[usize::try_from(a).unwrap()..usize::try_from(b).unwrap()].to_vec();
                }
                _ if !model.is_empty() => {
                    let i = rng.below(model.len() as u64);
                    let v = rng.leaf();
                    list = list.set(&heap, i, v.clone()).unwrap();
                    model[usize::try_from(i).unwrap()] = v;
                }
                _ => {}
            }
            assert!(
                list.well_formed(),
                "seed {seed}: unbalanced or mis-summarised"
            );
            assert_eq!(list.len(), model.len() as u64);
            let value = Value::new_list(list.clone());
            assert!(
                holds(&value),
                "seed {seed}: a labelled leaf under a clear summary"
            );
            let want = model.iter().fold(Labels::NONE, |l, v| l | v.labels);
            assert_eq!(value.scan(), want, "seed {seed}");
        }
        let got = list.to_vec();
        assert_eq!(got.len(), model.len());
        for (g, m) in got.iter().zip(model.iter()) {
            assert!(crate::ops::eq(g, m));
        }
        // The height stays logarithmic.
        assert!(u64::from(list.height()) <= 2 + 2 * u64::from(64 - list.len().leading_zeros()));
    }
}

#[test]
fn maps_agree_with_a_model_and_keep_their_summaries() {
    let heap = Heap::unmetered();
    for seed in 1..40u64 {
        let mut rng = Rng(seed * 104_729);
        let mut map = Map::empty();
        let mut model: BTreeMap<i64, Value> = BTreeMap::new();
        for _ in 0..300 {
            let k = i64::try_from(rng.below(60)).unwrap_or(0);
            if rng.below(3) == 0 {
                map = map.remove(&heap, &Value::int(k)).unwrap();
                model.remove(&k);
            } else {
                let v = rng.leaf();
                map = map.insert(&heap, Value::int(k), v.clone()).unwrap();
                model.insert(k, v);
            }
            assert!(map.well_formed(), "seed {seed}");
            assert_eq!(map.len(), model.len() as u64);
            assert!(holds(&Value::new_map(map.clone())), "seed {seed}");
        }
        let mut keys = Vec::new();
        map.for_each(&mut |k, _| keys.push(k.as_int().unwrap()));
        assert_eq!(keys, model.keys().copied().collect::<Vec<_>>());
    }
}

/// Random nested values: lists, tuples and records of leaves and of each other.
fn nested(rng: &mut Rng, depth: u32) -> Value {
    if depth == 0 || rng.below(3) == 0 {
        return rng.leaf();
    }
    let n = rng.below(4);
    let items: Vec<Value> = (0..n).map(|_| nested(rng, depth - 1)).collect();
    let l = rng.labels();
    match rng.below(3) {
        0 => Value::list(items).unwrap().with_labels(l),
        1 => Value::tuple(items).unwrap().with_labels(l),
        _ => {
            let fields: Vec<(String, Value)> = items
                .into_iter()
                .enumerate()
                .map(|(i, v)| (format!("f{i}"), v))
                .collect();
            Value::record(
                fields
                    .iter()
                    .map(|(n, v)| (n.as_str(), v.clone()))
                    .collect(),
            )
            .unwrap()
            .with_labels(l)
        }
    }
}

#[test]
fn no_container_with_a_labelled_leaf_has_a_clear_summary() {
    let heap = Heap::unmetered();
    for seed in 1..300u64 {
        let mut rng = Rng(seed * 31);
        let v = nested(&mut rng, 5);
        assert!(holds(&v), "seed {seed}");
        // The debug-build sink scan agrees with the summary bit.
        assert_eq!(crate::sink::check(&v).is_err(), !v.scan().is_empty());
        // Labels a replay adds (chapter 01: the union of the recorded labels and those the
        // replaying session's rules set) keep the invariant: they are added, never removed.
        let replayed = v.clone().with_labels(Labels::source(7));
        assert!(holds(&replayed) && replayed.scan().contains(v.scan() | Labels::source(7)));
        // Declassification removes every label, everywhere.
        let open = v.declassified(&heap).unwrap();
        assert!(open.scan().is_empty() && open.quick().is_empty());
        assert!(crate::ops::eq(&open, &v));
    }
}

/// TODO(stage 11b): replace with the real `ratio_cache` restore. A session cache entry stores
/// the value as JSON with the labels of every leaf; here the store and the restore are the
/// typeless JSON parse of the stored text with its labels put back on, the representation
/// stage 11b's entry will use. The property is the same: a value cached while labelled comes
/// back labelled, and its summary holds.
#[test]
fn values_restored_from_a_cache_entry_keep_labels_stub() {
    let heap = Heap::unmetered();
    for seed in 1..100u64 {
        let mut rng = Rng(seed * 131);
        let v = nested(&mut rng, 4);
        let labels = v.scan();
        let mut text = String::new();
        let p = crate::prepare::empty_for_tests();
        crate::json::write(&p, &v, &mut text);
        let restored = crate::json::parse(&heap, &text, labels).unwrap();
        assert!(holds(&restored), "seed {seed}");
        assert!(
            restored.scan().contains(labels),
            "seed {seed}: a restored value lost a label"
        );
    }
}
