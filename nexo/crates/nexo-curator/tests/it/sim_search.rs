#![cfg(feature = "sim")]
//! The knob search of the session simulator (`nexo_curator::sim::search`).

use nexo_curator::sim::search::{cli, collapsed_axes, coordinate_descent, lhs, pareto, Config, Space};

const KNOBS: &str = r#"
[float]
threshold = [0.005, 0.1]
ratio = [0.02, 0.5]
margin = [1.0, 6.0]
[int]
protect = [4, 40]
cap = [0, 200000]
still = [0, 128000]
[bool]
live
pockets
[enum]
cut = ["age", "need", "copies", "cover"]
"#;

fn space() -> Space {
    Space::parse(KNOBS).expect("the knob file parses")
}

/// Quadratic in `protect` and `threshold`, minimum 1.0 at protect 16, threshold 0.03.
fn toy(space: &Space) -> impl Fn(&Config) -> f64 + Sync + '_ {
    let (p, t) = (space.index("protect").unwrap(), space.index("threshold").unwrap());
    move |c: &Config| 1.0 + ((c[p] - 16.0) / 36.0).powi(2) + ((c[t] - 0.03) / 0.095).powi(2)
}

#[test]
fn coordinate_descent_finds_planted_optimum() {
    let space = space();
    let obj = toy(&space);
    let start = space.default_config();
    let j0 = obj(&start);
    let d = coordinate_descent(&space, start, &obj, 3, 7);
    assert!(d.sweeps <= 3, "{} sweeps", d.sweeps);
    assert!(d.j <= 1.0 * 1.01, "J {} is not within 1 % of the minimum 1.0 (start {j0})", d.j);
    assert!(d.j < j0);
    // the spec string is what a bench arm takes
    let spec = space.render(&d.config);
    assert!(spec.contains("directives=0;nudge=0;recall=1;rule=rent;head=turn"), "{spec}");
    assert!(spec.contains("protect="), "{spec}");

    // the same seed walks the same way
    let again = coordinate_descent(&space, space.default_config(), &obj, 3, 7);
    assert_eq!(space.render(&again.config), spec);
}

#[test]
fn lhs_with_the_same_seed_gives_the_same_ten_configs() {
    let space = space();
    let (a, b) = (lhs(&space, 10, 42), lhs(&space, 10, 42));
    assert_eq!(a.len(), 10);
    assert_eq!(a, b);
    assert_ne!(a, lhs(&space, 10, 43));
    // every value sits inside its range and has the type of its knob
    for c in &a {
        assert!((0.005..=0.1).contains(&c[space.index("threshold").unwrap()]));
        let p = c[space.index("protect").unwrap()];
        assert!((4.0..=40.0).contains(&p) && p.fract() == 0.0);
        let l = c[space.index("live").unwrap()];
        assert!(l == 0.0 || l == 1.0);
        assert!(c[space.index("cut").unwrap()] < 4.0);
    }
    // latin hypercube: each tenth of an int range holds exactly one of the 10 samples
    let k = space.index("still").unwrap();
    let mut bins: Vec<usize> = a.iter().map(|c| ((c[k] / 128001.0) * 10.0) as usize).collect();
    bins.sort();
    assert_eq!(bins, (0..10).collect::<Vec<_>>());
}

#[test]
fn render_and_default_round_trip() {
    let space = space();
    let d = space.default_config();
    let spec = space.render(&d);
    assert!(spec.contains("protect=12") && spec.contains("threshold=0.02") && spec.contains("cut=age"), "{spec}");
    assert_eq!(space.parse_spec(&spec).unwrap(), d);
}

#[test]
fn pareto_keeps_the_non_dominated() {
    // (cost, extra steps, p fail)
    let pts = [[1.0, 5.0, 0.1], [2.0, 1.0, 0.1], [2.0, 5.0, 0.2], [1.0, 5.0, 0.1], [0.5, 9.0, 0.3]];
    assert_eq!(pareto(&pts), vec![0, 1, 3, 4]);
}

#[test]
fn pareto_layers_fill_up_to_the_wanted_count() {
    use nexo_curator::sim::search::pareto_layers;
    // index 0 is the reference; 1 and 2 form the front, 3 and 4 the second front
    let pts = [[9.0, 9.0, 9.0], [1.0, 2.0, 0.0], [2.0, 1.0, 0.0], [2.0, 3.0, 0.0], [3.0, 2.0, 0.0]];
    let two = pareto_layers(&pts, 2);
    assert!(two.iter().all(|&(_, r)| r == 1) && two.len() == 2);
    let four = pareto_layers(&pts, 4);
    assert_eq!(four.iter().filter(|&&(_, r)| r == 2).count(), 2);
}

#[test]
fn an_axis_without_spread_is_named() {
    let pts = [[1.0, 0.0, 0.0], [2.0, 0.0, 0.0], [1.5, 0.0, 0.0]];
    assert_eq!(collapsed_axes(&pts), ["extra_steps", "p_fail"]);
    assert!(collapsed_axes(&[[1.0, 2.0, 0.1], [2.0, 1.0, 0.2]]).is_empty());
    assert!(collapsed_axes(&[[1.0, 0.0, 0.0]]).is_empty());
}

#[test]
fn a_missing_fixture_stops_the_search_unless_allowed() {
    let dir = std::env::temp_dir().join(format!("sim-search-missing-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let knobs = dir.join("knobs.toml");
    std::fs::write(&knobs, KNOBS).unwrap();
    let args = |extra: &[&str]| {
        let mut v: Vec<String> =
            ["--knobs", knobs.to_str().unwrap(), "--fixtures", dir.to_str().unwrap(), "--configs", "2", "--out"]
                .map(String::from)
                .to_vec();
        v.push(dir.join("out").to_str().unwrap().to_string());
        v.extend(extra.iter().map(|s| s.to_string()));
        v.into_iter()
    };
    let e = cli(args(&[])).expect_err("no fixture is there");
    assert!(e.contains("not found in") && e.contains("--allow-missing"), "{e}");
    let e = cli(args(&["--allow-missing"])).expect_err("still nothing to search on");
    assert!(e.contains("no fixture of tier fast"), "{e}");
    std::fs::remove_dir_all(&dir).ok();
}
