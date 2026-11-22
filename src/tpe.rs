//! Choosing the next values of an optimization from the results so far (docs/data-management.md,
//! 11.7): random search and the Tree-structured Parzen Estimator (TPE).
//!
//! TPE splits the trials so far into the better ones and the rest. For one parameter it makes
//! a density of the values the better trials had and one of the values the rest had, draws a
//! few values from the first, and takes the one where the first is high and the second is low.
//! Each parameter is chosen on its own, from the trials in which it was used, which is how a
//! parameter that only matters for one option is handled.
//!
//! It is written here rather than taken from a library: what is needed is small, and the same
//! seed and the same results have to give the same values in every version, so that a trial
//! that was made before is found by its key and not made again. The random numbers are made
//! here for the same reason.

/// A generator of random numbers that gives the same numbers for the same seed, everywhere
/// (SplitMix64).
#[derive(Clone, Debug)]
pub struct Random(u64);

impl Random {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A number in [0, 1).
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// A whole number below `count`, which is above zero.
    pub fn below(&mut self, count: usize) -> usize {
        ((self.unit() * count as f64) as usize).min(count - 1)
    }

    /// A number of the standard normal distribution (Box–Muller).
    pub fn normal(&mut self) -> f64 {
        let first = self.unit().max(f64::MIN_POSITIVE);
        let second = self.unit();
        (-2.0 * first.ln()).sqrt() * (std::f64::consts::TAU * second).cos()
    }
}

/// What a parameter can be.
#[derive(Clone, Debug, PartialEq)]
pub enum Domain {
    /// One of `count` choices, by their position.
    Choice(usize),
    /// A whole number from `low` to `high`, both included. With `log`, the values are tried as if
    /// spread evenly on a logarithmic scale: 100 to 200 is as far as 800 to 1600.
    Int { low: i64, high: i64, log: bool },
}

/// A value of a parameter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Picked {
    Choice(usize),
    Int(i64),
}

/// How the next values are chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    /// Every value as likely as any other.
    Random,
    /// TPE, after `startup` trials in which the parameter was used, chosen at random.
    Tpe { startup: usize },
}

/// How many values TPE draws to choose the next one from.
const CANDIDATES: usize = 24;
/// The share of trials taken as the better ones.
const GAMMA: f64 = 0.25;
/// At most this many trials are taken as the better ones.
const MOST_GOOD: usize = 25;

/// The value of one parameter for the next trial.
///
/// `history` holds the trials in which the parameter was used, with their value, the best
/// first: TPE takes the first quarter as the better ones. A trial that failed belongs at the
/// end.
pub fn suggest(domain: &Domain, history: &[Picked], method: Method, random: &mut Random) -> Picked {
    let startup = match method {
        Method::Random => usize::MAX,
        Method::Tpe { startup } => startup.max(1),
    };
    if history.len() < startup.max(2) {
        return uniform(domain, random);
    }
    let good = ((history.len() as f64 * GAMMA).ceil() as usize).clamp(1, MOST_GOOD);
    let (better, rest) = history.split_at(good.min(history.len() - 1));
    match domain {
        Domain::Choice(count) => {
            let better = Histogram::new(*count, better);
            let rest = Histogram::new(*count, rest);
            best_of(random, |random| {
                let choice = better.draw(random);
                (
                    Picked::Choice(choice),
                    better.log(choice) - rest.log(choice),
                )
            })
        }
        Domain::Int { low, high, log } => {
            let scale = Scale::new(*low, *high, *log);
            let better = Parzen::new(&scale, better);
            let rest = Parzen::new(&scale, rest);
            best_of(random, |random| {
                let x = better.draw(&scale, random);
                (
                    Picked::Int(scale.value(x)),
                    better.log(&scale, x) - rest.log(&scale, x),
                )
            })
        }
    }
}

/// A value drawn evenly.
pub fn uniform(domain: &Domain, random: &mut Random) -> Picked {
    match domain {
        Domain::Choice(count) => Picked::Choice(random.below(*count)),
        Domain::Int { low, high, log } => {
            let scale = Scale::new(*low, *high, *log);
            Picked::Int(scale.value(scale.low + random.unit() * (scale.high - scale.low)))
        }
    }
}

/// Draws `CANDIDATES` values and keeps the one with the highest score.
fn best_of(random: &mut Random, mut draw: impl FnMut(&mut Random) -> (Picked, f64)) -> Picked {
    let mut best = draw(random);
    for _ in 1..CANDIDATES {
        let next = draw(random);
        if next.1 > best.1 {
            best = next;
        }
    }
    best.0
}

/// How often each choice was taken, with one more for each so that none is ruled out.
struct Histogram(Vec<f64>);

impl Histogram {
    fn new(count: usize, history: &[Picked]) -> Self {
        let mut weights = vec![1.0; count];
        for picked in history {
            if let Picked::Choice(choice) = picked
                && *choice < count
            {
                weights[*choice] += 1.0;
            }
        }
        let total: f64 = weights.iter().sum();
        Self(weights.into_iter().map(|weight| weight / total).collect())
    }

    fn log(&self, choice: usize) -> f64 {
        self.0[choice].ln()
    }

    fn draw(&self, random: &mut Random) -> usize {
        let mut left = random.unit();
        for (choice, share) in self.0.iter().enumerate() {
            if left < *share {
                return choice;
            }
            left -= share;
        }
        self.0.len() - 1
    }
}

/// Where whole numbers are placed to be compared: their own scale or their logarithm, and the
/// interval that holds each of them whole.
struct Scale {
    low: f64,
    high: f64,
    log: bool,
    bounds: (i64, i64),
}

impl Scale {
    fn new(low: i64, high: i64, log: bool) -> Self {
        let (low, high) = (low.min(high), low.max(high));
        let log = log && low > 0;
        let place = |value: f64| if log { value.ln() } else { value };
        Self {
            low: place(low as f64 - 0.5).max(if log { (0.5f64).ln() } else { f64::MIN }),
            high: place(high as f64 + 0.5),
            log,
            bounds: (low, high),
        }
    }

    fn place(&self, value: i64) -> f64 {
        if self.log {
            (value as f64).ln()
        } else {
            value as f64
        }
    }

    fn value(&self, x: f64) -> i64 {
        let value = if self.log { x.exp() } else { x };
        (value.round() as i64).clamp(self.bounds.0, self.bounds.1)
    }
}

/// A density made of one normal distribution around each value seen, and a wide one over the
/// whole interval, each cut to the interval.
struct Parzen {
    /// Centre and width of each part. All parts weigh the same.
    parts: Vec<(f64, f64)>,
}

impl Parzen {
    fn new(scale: &Scale, history: &[Picked]) -> Self {
        let span = scale.high - scale.low;
        let mut centres: Vec<f64> = history
            .iter()
            .filter_map(|picked| match picked {
                Picked::Int(value) => Some(scale.place(*value)),
                Picked::Choice(_) => None,
            })
            .collect();
        centres.push(scale.low + span / 2.0);
        let prior = centres.len() - 1;
        let mut sorted = centres.clone();
        sorted.sort_by(f64::total_cmp);
        // The width of a part is the distance to its farther neighbour, within limits.
        let narrowest = span / (centres.len() as f64 + 1.0).min(100.0);
        let parts = centres
            .iter()
            .enumerate()
            .map(|(position, centre)| {
                if position == prior {
                    return (*centre, span);
                }
                let at = sorted.partition_point(|value| value < centre);
                let left = if at == 0 {
                    centre - scale.low
                } else {
                    centre - sorted[at - 1]
                };
                let right = sorted
                    .get(at + 1)
                    .map_or(scale.high - centre, |next| next - centre);
                (*centre, left.max(right).clamp(narrowest, span))
            })
            .collect();
        Self { parts }
    }

    fn draw(&self, scale: &Scale, random: &mut Random) -> f64 {
        let (centre, width) = self.parts[random.below(self.parts.len())];
        for _ in 0..100 {
            let x = centre + width * random.normal();
            if (scale.low..=scale.high).contains(&x) {
                return x;
            }
        }
        centre.clamp(scale.low, scale.high)
    }

    fn log(&self, scale: &Scale, x: f64) -> f64 {
        let density: f64 = self
            .parts
            .iter()
            .map(|(centre, width)| {
                let inside = normal_cdf((scale.high - centre) / width)
                    - normal_cdf((scale.low - centre) / width);
                let z = (x - centre) / width;
                (-0.5 * z * z).exp()
                    / (width * (std::f64::consts::TAU).sqrt() * inside.max(f64::MIN_POSITIVE))
            })
            .sum::<f64>()
            / self.parts.len() as f64;
        density.max(f64::MIN_POSITIVE).ln()
    }
}

fn normal_cdf(z: f64) -> f64 {
    0.5 * (1.0 + erf(z / std::f64::consts::SQRT_2))
}

/// The error function, to within 1.5e-7 (Abramowitz and Stegun, 7.1.26).
fn erf(x: f64) -> f64 {
    let sign = x.signum();
    let x = x.abs();
    let t = 1.0 / (1.0 + 0.327_591_1 * x);
    let y = 1.0
        - (((((1.061_405_429 * t - 1.453_152_027) * t) + 1.421_413_741) * t - 0.284_496_736) * t
            + 0.254_829_592)
            * t
            * (-x * x).exp();
    sign * y
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Finds the best value of `score` in `trials` trials, as an optimization does: the history
    /// is kept best first.
    fn search(
        domain: &Domain,
        method: Method,
        seed: u64,
        trials: usize,
        score: impl Fn(Picked) -> f64,
    ) -> f64 {
        let mut random = Random::new(seed);
        let mut history: Vec<(Picked, f64)> = Vec::new();
        for _ in 0..trials {
            let picked: Vec<Picked> = history.iter().map(|(picked, _)| *picked).collect();
            let next = suggest(domain, &picked, method, &mut random);
            let value = score(next);
            let at = history.partition_point(|(_, seen)| *seen >= value);
            history.insert(at, (next, value));
        }
        history[0].1
    }

    #[test]
    fn the_same_seed_gives_the_same_values() {
        let domain = Domain::Int {
            low: 50,
            high: 2000,
            log: true,
        };
        let run = |seed| {
            let mut random = Random::new(seed);
            let history = [
                Picked::Int(400),
                Picked::Int(800),
                Picked::Int(100),
                Picked::Int(1600),
            ];
            (0..10)
                .map(|_| suggest(&domain, &history, Method::Tpe { startup: 3 }, &mut random))
                .collect::<Vec<_>>()
        };
        assert_eq!(run(7), run(7));
        assert_ne!(run(7), run(8));
        // The numbers themselves do not change between versions.
        let mut random = Random::new(1);
        assert_eq!(random.next_u64(), 0x910a_2dec_8902_5cc1);
    }

    #[test]
    fn values_stay_inside_their_domain() {
        let mut random = Random::new(3);
        for domain in [
            Domain::Int {
                low: 1,
                high: 1,
                log: false,
            },
            Domain::Int {
                low: 0,
                high: 7,
                log: true,
            },
            Domain::Int {
                low: 10,
                high: 20,
                log: false,
            },
            Domain::Choice(1),
            Domain::Choice(5),
        ] {
            let history: Vec<Picked> = (0..12).map(|_| uniform(&domain, &mut random)).collect();
            for _ in 0..200 {
                match (
                    suggest(&domain, &history, Method::Tpe { startup: 2 }, &mut random),
                    &domain,
                ) {
                    (Picked::Int(value), Domain::Int { low, high, .. }) => {
                        assert!((*low..=*high).contains(&value), "{value} in {domain:?}");
                    }
                    (Picked::Choice(choice), Domain::Choice(count)) => assert!(choice < *count),
                    (picked, _) => panic!("{picked:?} for {domain:?}"),
                }
            }
        }
    }

    #[test]
    fn finds_a_good_whole_number_sooner_than_random_search() {
        let domain = Domain::Int {
            low: 50,
            high: 3200,
            log: true,
        };
        // Best at 700; the score falls with the distance on a logarithmic scale.
        let score = |picked| match picked {
            Picked::Int(value) => -((value as f64).ln() - 700f64.ln()).abs(),
            Picked::Choice(_) => f64::MIN,
        };
        let (mut tpe, mut random) = (0.0, 0.0);
        for seed in 0..40 {
            tpe += search(&domain, Method::Tpe { startup: 5 }, seed, 20, score);
            random += search(&domain, Method::Random, seed, 20, score);
        }
        assert!(tpe > random, "TPE {tpe:.3}, random {random:.3}");
    }

    /// How often the trials after the first ones take one of the `good` choices.
    fn later_on_good(method: Method, seed: u64, good: &[usize]) -> usize {
        let domain = Domain::Choice(6);
        let score = |choice: usize| if good.contains(&choice) { 1.0 } else { 0.0 };
        let mut random = Random::new(seed);
        let mut history: Vec<(Picked, f64)> = Vec::new();
        let mut taken = 0;
        for trial in 0..20 {
            let picked: Vec<Picked> = history.iter().map(|(picked, _)| *picked).collect();
            let Picked::Choice(choice) = suggest(&domain, &picked, method, &mut random) else {
                unreachable!()
            };
            if trial >= 6 {
                taken += usize::from(good.contains(&choice));
            }
            let value = score(choice);
            let at = history.partition_point(|(_, seen)| *seen >= value);
            history.insert(at, (Picked::Choice(choice), value));
        }
        taken
    }

    /// Choices have no order, so TPE cannot guess a choice it has not seen. What it does is
    /// take the choices that did well more often than the others.
    #[test]
    fn tries_the_choices_that_did_well_more_often_than_random_search() {
        let (mut tpe, mut random) = (0, 0);
        for seed in 0..40 {
            tpe += later_on_good(Method::Tpe { startup: 6 }, seed, &[2]);
            random += later_on_good(Method::Random, seed, &[2]);
        }
        // Random search takes the good choice in one trial of six.
        assert!(tpe > random * 2, "TPE {tpe}, random {random}");
    }
}
