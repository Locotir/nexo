use super::*;

/// A point of the knob space, one number per knob in the order of the space (an enum is its index).
pub type Config = Vec<f64>;

/// What a knob is.
#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    Float(f64, f64),
    Int(i64, i64),
    Bool,
    Enum(Vec<String>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Knob {
    pub name: String,
    pub kind: Kind,
}

/// Settings fixed by rule, never searched (`SIMULATOR-SPEC.md` 4.1).
pub const FIXED: &str = "directives=0;nudge=0;recall=1;rule=rent;head=turn";

#[derive(Debug, Clone)]
pub struct Space {
    pub knobs: Vec<Knob>,
}

/// A xorshift64* generator: a seed gives the same numbers everywhere.
#[derive(Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        let mut r = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0x2545_F491_4F6C_DD1D);
        if r.0 == 0 {
            r.0 = 0x1234_5678_9ABC_DEF1;
        }
        for _ in 0..4 {
            r.next_u64();
        }
        r
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in [0, 1).
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

fn list(v: &str) -> Vec<String> {
    v.trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(|s| s.trim().trim_matches('"').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

impl Space {
    /// Reads `knobs.toml`: sections `[float]` and `[int]` with `name = [lo, hi]`, `[bool]` with one name per
    /// line, `[enum]` with `name = ["a", "b"]`. `#` starts a comment.
    pub fn parse(text: &str) -> Result<Space, String> {
        let (mut section, mut knobs) = (String::new(), Vec::new());
        for (n, raw) in text.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            if let Some(s) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                section = s.trim().to_string();
                continue;
            }
            let at = |m: &str| format!("knobs line {}: {m}", n + 1);
            let (name, val) = line.split_once('=').map_or((line, ""), |(a, b)| (a.trim(), b.trim()));
            let nums = || -> Result<(f64, f64), String> {
                let v = list(val);
                match (v.first().map(|s| s.parse::<f64>()), v.get(1).map(|s| s.parse::<f64>())) {
                    (Some(Ok(a)), Some(Ok(b))) if a <= b => Ok((a, b)),
                    _ => Err(at("expected [lo, hi]")),
                }
            };
            let kind = match section.as_str() {
                "float" => {
                    let (a, b) = nums()?;
                    Kind::Float(a, b)
                }
                "int" => {
                    let (a, b) = nums()?;
                    Kind::Int(a as i64, b as i64)
                }
                "bool" => Kind::Bool,
                "enum" => {
                    let v = list(val);
                    if v.is_empty() {
                        return Err(at("an enum needs values"));
                    }
                    Kind::Enum(v)
                }
                other => return Err(at(&format!("unknown section [{other}]"))),
            };
            if knobs.iter().any(|k: &Knob| k.name == name) {
                return Err(at(&format!("{name} twice")));
            }
            knobs.push(Knob { name: name.to_string(), kind });
        }
        if knobs.is_empty() {
            return Err("knobs: no knob found".into());
        }
        Ok(Space { knobs })
    }

    pub fn index(&self, name: &str) -> Option<usize> {
        self.knobs.iter().position(|k| k.name == name)
    }

    /// `v` made a legal value of knob `k`: clamped, rounded to 4 significant digits (floats) or to a whole
    /// number, so a config and its spec string are the same thing.
    pub fn quantize(&self, k: usize, v: f64) -> f64 {
        match &self.knobs[k].kind {
            Kind::Float(lo, hi) => format!("{:.3e}", v.clamp(*lo, *hi)).parse::<f64>().unwrap_or(*lo).clamp(*lo, *hi),
            Kind::Int(lo, hi) => v.round().clamp(*lo as f64, *hi as f64),
            Kind::Bool => f64::from(v >= 0.5),
            Kind::Enum(e) => v.round().clamp(0.0, (e.len() - 1) as f64),
        }
    }

    /// The config a spec string describes, every knob not in `spec` taken from `D_SPEC`.
    pub fn parse_spec(&self, spec: &str) -> Result<Config, String> {
        let read = |s: &str| -> BTreeMap<String, String> {
            s.split(';')
                .filter_map(|kv| kv.split_once('='))
                .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
                .collect()
        };
        let (given, base) = (read(spec), read(crate::D_SPEC));
        let mut c = Vec::new();
        for (k, knob) in self.knobs.iter().enumerate() {
            let raw = given.get(&knob.name).or_else(|| base.get(&knob.name));
            let v = match (&knob.kind, raw) {
                (Kind::Enum(e), Some(s)) => {
                    e.iter().position(|x| x == s).ok_or(format!("{}={s} is not allowed", knob.name))? as f64
                }
                (Kind::Enum(_), None) => 0.0,
                (Kind::Float(lo, _), None) => *lo,
                (Kind::Int(lo, _), None) => *lo as f64,
                (Kind::Bool, None) => 0.0,
                (_, Some(s)) => {
                    s.replace('_', "").parse::<f64>().map_err(|_| format!("{}={s} is not a number", knob.name))?
                }
            };
            c.push(self.quantize(k, v));
        }
        Ok(c)
    }

    /// `D_SPEC` as a config.
    pub fn default_config(&self) -> Config {
        self.parse_spec(crate::D_SPEC).expect("D_SPEC holds every knob")
    }

    fn value(&self, k: usize, v: f64) -> String {
        match &self.knobs[k].kind {
            Kind::Float(..) => format!("{v}"),
            Kind::Int(..) | Kind::Bool => format!("{}", v as i64),
            Kind::Enum(e) => e[v as usize].clone(),
        }
    }

    /// The spec string of a config: the fixed settings, then every knob, over `D_SPEC`. It pastes into
    /// `nexo bench --arms curator:<spec>`.
    pub fn render(&self, c: &Config) -> String {
        let mut s = FIXED.to_string();
        for (k, knob) in self.knobs.iter().enumerate() {
            let _ = write!(s, ";{}={}", knob.name, self.value(k, c[k]));
        }
        s
    }

    fn at_unit(&self, k: usize, u: f64) -> f64 {
        let v = match &self.knobs[k].kind {
            Kind::Float(lo, hi) => lo + u * (hi - lo),
            Kind::Int(lo, hi) => (*lo as f64 + (u * (hi - lo + 1) as f64).floor()).min(*hi as f64),
            Kind::Bool => f64::from(u >= 0.5),
            Kind::Enum(e) => (u * e.len() as f64).floor().min((e.len() - 1) as f64),
        };
        self.quantize(k, v)
    }
}

/// `n` configs by Latin hypercube sampling: every knob's range is cut into `n` strata and each stratum is
/// used once, in an order drawn per knob. The same seed gives the same configs.
pub fn lhs(space: &Space, n: usize, seed: u64) -> Vec<Config> {
    let mut rng = Rng::new(seed);
    let mut out = vec![vec![0.0; space.knobs.len()]; n];
    for k in 0..space.knobs.len() {
        let mut perm: Vec<usize> = (0..n).collect();
        for i in (1..n).rev() {
            perm.swap(i, (rng.next_u64() % (i as u64 + 1)) as usize);
        }
        for (i, row) in out.iter_mut().enumerate() {
            let u = (perm[i] as f64 + rng.unit()) / n as f64;
            row[k] = space.at_unit(k, u);
        }
    }
    out
}

// ---- coordinate descent ----
